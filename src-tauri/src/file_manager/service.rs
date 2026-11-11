use super::types::*;
use crate::{AppError, PeerManager, cloud::runtime::CloudRuntime};
use std::{sync::Arc, time::Instant};

pub(crate) fn directory_entry(id: String, parent_id: String, name: String) -> FileEntryDto {
    FileEntryDto { id, parent_id, name, is_dir: true, size: 0, modified: None, content_type: None, online_preview: false, protected: false }
}

pub(crate) fn path_breadcrumbs(path: &str) -> Vec<FileBreadcrumbDto> {
    let absolute = path.starts_with('/');
    let mut current = if absolute { "/".to_owned() } else { String::new() };
    path.split('/').filter(|segment| !segment.is_empty()).map(|name| {
        if absolute && current != "/" { current.push('/'); }
        current.push_str(name);
        if !absolute { current.push('/'); }
        FileBreadcrumbDto { id: current.clone(), name: name.into() }
    }).collect()
}

pub(crate) fn ensure_delete_target(id: &str, root_id: &str) -> Result<(), AppError> {
    if id == root_id {
        return Err(AppError { code: "PEER_DELETE_ROOT_FORBIDDEN".into(), message: "不能删除根目录，请选择其中的文件或目录 / Cannot delete the root; select its children".into(), detail: None, context: None });
    }
    // IDs are opaque. Canonicalization and path validation belong to adapters;
    // trimming an ID here can alias a legitimate entry with the root.
    if id.is_empty() { return Err(invalid()); }
    Ok(())
}

pub(crate) fn unavailable() -> AppError {
    AppError { code: "FILE_CAPABILITY_UNAVAILABLE".into(), message: "此操作暂不支持 / Operation unavailable".into(), detail: None, context: None }
}

pub(crate) fn invalid() -> AppError {
    AppError {
        code: "FILE_SESSION_INVALID".into(),
        message: "文件会话已失效，请重新打开文件管理器 / File session is invalid; reopen the file browser".into(),
        detail: None,
        context: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn delete_failures_serialize_opaque_ids_without_protocol_keys() -> Result<(), serde_json::Error> {
        let result = FileDeleteResultDto {
            deleted: vec!["deleted-id".into()],
            failed: vec![FileDeleteFailureDto { id: "opaque/../id".into(), error: invalid() }],
        };
        let wire = serde_json::to_value(result)?;
        assert_eq!(wire["failed"][0]["id"], "opaque/../id");
        assert!(wire["failed"][0].get("key").is_none());
        assert_eq!(wire["failed"][0]["error"]["code"], "FILE_SESSION_INVALID");
        Ok(())
    }
    #[test]
    fn root_identity_is_exact_and_other_ids_remain_opaque() {
        for root in ["", "/", "selected/"] {
            assert!(ensure_delete_target(root, root).is_err());
            assert!(ensure_delete_target("", root).is_err());
            assert!(ensure_delete_target("child-id", root).is_ok());
        }
        assert!(ensure_delete_target("selected", "selected/").is_ok());
        assert!(ensure_delete_target("opaque/../id", "root").is_ok());
    }
}

impl FileSessions {
    /// Factories own protocol admission and adaptation; session dispatch has no
    /// provider branches and publishes only the shared browser contract.
    pub async fn open(
        &self,
        peers: &PeerManager,
        cloud: &CloudRuntime,
        location: FileLocation,
    ) -> Result<FileSessionDto, AppError> {
        for factory in super::factory::factories() {
            if let Some((backend, root)) = factory.create(peers, cloud, &location).await? {
                return self.insert_session(backend, root, location).await;
            }
        }
        Err(invalid())
    }

    async fn insert_session(
        &self,
        backend: Arc<dyn super::traits::BrowserBackend>,
        root: String,
        location: FileLocation,
    ) -> Result<FileSessionDto, AppError> {
        let mut sessions = self.sessions.lock().await;
        sessions.retain(|_, session| session.created.elapsed().as_secs() < FILE_SESSION_TTL_SECS);
        if sessions.len() >= MAX_FILE_SESSIONS {
            return Err(invalid());
        }
        let session_id = crate::local_upload::token()?;
        let capabilities = backend.capabilities();
        let supports_move = backend.supports_move();
        sessions.insert(
            session_id.clone(),
            Arc::new(FileSession {
                root: root.clone(),
                location,
                backend,
                created: Instant::now(),
            }),
        );
        Ok(FileSessionDto {
            session_id,
            supports_move,
            root,
            capabilities,
        })
    }

    pub(crate) async fn admitted(
        &self,
        peers: &PeerManager,
        cloud: &CloudRuntime,
        id: &str,
    ) -> Result<Arc<FileSession>, AppError> {
        let session = self
            .sessions
            .lock()
            .await
            .get(id)
            .cloned()
            .ok_or_else(invalid)?;
        if session.created.elapsed().as_secs() >= FILE_SESSION_TTL_SECS {
            return Err(invalid());
        }
        session.backend.admit(peers, cloud).await?;
        if !self
            .sessions
            .lock()
            .await
            .get(id)
            .is_some_and(|current| Arc::ptr_eq(current, &session))
        {
            return Err(invalid());
        }
        Ok(session)
    }

    pub async fn list(
        &self,
        peers: &PeerManager,
        cloud: &CloudRuntime,
        id: &str,
        path: &str,
        next: Option<&str>,
    ) -> Result<FilePageDto, AppError> {
        let session = self.admitted(peers, cloud, id).await?;
        let mut page = session.backend.list(peers, cloud, path, next).await?;
        let root_name = match &session.location {
            FileLocation::Peerdav { peer_id, share_id } => peers.shares(peer_id).await.map_err(AppError::from)?.into_iter().find(|share| share.id == *share_id).map(|share| share.name).ok_or_else(invalid)?,
            FileLocation::Cloud { connection_id, root_id } => {
                let config = cloud.config.snapshot().map_err(crate::cloud::runtime::config_error)?;
                if let Some(connection) = config.official_connections.iter().find(|entry| entry.id == *connection_id) { connection.name.clone() }
                else { root_id.clone().filter(|id| !id.is_empty()).unwrap_or(cloud.connection(connection_id)?.config.bucket) }
            }
            FileLocation::S3 { bucket, .. } => bucket.clone(),
        };
        page.breadcrumbs.retain(|crumb| crumb.id != session.root);
        page.breadcrumbs.insert(0, FileBreadcrumbDto { id: session.root.clone(), name: root_name });
        // Shared presentation contract, independent of provider listing order.
        page.entries.sort_by_key(|entry| !entry.is_dir);
        // Reconfiguration while I/O was in flight must not publish old data into
        // a newly authorized session. Underlying operations retain their guards.
        self.admitted(peers, cloud, id).await?;
        Ok(page)
    }

    pub async fn preview(
        &self,
        peers: &PeerManager,
        cloud: &CloudRuntime,
        id: &str,
        path: &str,
    ) -> Result<crate::PreviewPayload, AppError> {
        let session = self.admitted(peers, cloud, id).await?;
        let result = session.backend.preview(peers, cloud, path).await?;
        self.admitted(peers, cloud, id).await?;
        Ok(result)
    }

    pub async fn delete_plan(
        &self,
        peers: &PeerManager,
        cloud: &CloudRuntime,
        id: &str,
        path: &str,
    ) -> Result<FileDeletePlanDto, AppError> {
        let session = self.admitted(peers, cloud, id).await?;
        ensure_delete_target(path, &session.root)?;
        if !session.backend.capabilities().delete { return Err(unavailable()); }
        let plan = session.backend.delete_plan(peers, cloud, path).await?;
        let confirmation = session.backend.capabilities().delete_confirmation;
        self.admitted(peers, cloud, id).await?;
        let token = crate::local_upload::token()?;
        let provider_plan = (!plan.plan_id.is_empty()).then(|| plan.plan_id.clone());
        let mut plans = self.delete_plans.lock().await;
        plans.retain(|_, plan| plan.created.elapsed().as_secs() < 300);
        if plans.len() >= MAX_FILE_SESSIONS {
            return Err(invalid());
        }
        plans.insert(
            token.clone(),
            FileDeletePlan {
                session,
                target: path.to_owned(),
                provider_plan,
                created: Instant::now(),
            },
        );
        Ok(FileDeletePlanDto {
            plan_id: token,
            target_id: path.to_owned(),
            confirmation,
            expires_at_unix: plan.expires_at_unix,
            object_count: plan.object_count,
            entries: plan.entries.into_iter().map(|entry| FileDeleteEntryDto {
                id: entry.key, size: entry.size, revision: entry.etag,
            }).collect(),
        })
    }

    pub async fn delete_execute(
        &self,
        peers: &PeerManager,
        cloud: &CloudRuntime,
        id: &str,
        plan_id: &str,
        confirmed: bool,
    ) -> Result<FileDeleteResultDto, AppError> {
        let session = self.admitted(peers, cloud, id).await?;
        if !confirmed {
            return Err(invalid());
        }
        let plan = {
            let mut plans = self.delete_plans.lock().await;
            let plan = plans.get(plan_id).ok_or_else(invalid)?;
            if plan.created.elapsed().as_secs() >= 300 || !Arc::ptr_eq(&plan.session, &session) {
                return Err(invalid());
            }
            plans.remove(plan_id).ok_or_else(invalid)?
        };
        ensure_delete_target(&plan.target, &session.root)?;
        let result = session
            .backend
            .delete_execute(peers, cloud, &plan.target, plan.provider_plan.as_deref())
            .await?;
        Ok(FileDeleteResultDto {
            deleted: result.deleted,
            failed: result.failed.into_iter().map(|failure| FileDeleteFailureDto { id: failure.key, error: failure.error }).collect(),
        })
    }

    pub async fn move_entry(&self, peers: &PeerManager, cloud: &CloudRuntime, session_id: &str, id: &str, destination: &str) -> Result<(), AppError> {
        let session = self.admitted(peers, cloud, session_id).await?;
        if !session.backend.supports_move() { return Err(unavailable()); }
        ensure_delete_target(id, &session.root)?;
        session.backend.move_entry(peers, cloud, id, destination).await
    }

    pub async fn create_directory(&self, peers: &PeerManager, cloud: &CloudRuntime, id: &str, path: &str, name: &str) -> Result<FileEntryDto, AppError> {
        if name.trim().is_empty() || name.len() > 255 || name.contains(['/', '\\', '\0', ':']) || matches!(name, "." | "..") { return Err(invalid()); }
        let session = self.admitted(peers, cloud, id).await?;
        if !session.backend.capabilities().create_dir { return Err(unavailable()); }
        let entry = session.backend.create_dir(peers, cloud, path, name).await?;
        self.admitted(peers, cloud, id).await?;
        Ok(entry)
    }

    pub async fn close(&self, id: &str) {
        if let Some(session) = self.sessions.lock().await.remove(id) {
            self.delete_plans
                .lock()
                .await
                .retain(|_, plan| !Arc::ptr_eq(&plan.session, &session));
        }
    }
}
