use crate::{AppError, DomainError, download_types::*, traits::TransferQueueAdapter};
use futures_util::future::BoxFuture;
use std::sync::Arc;
fn app(e: crate::cloud::ObjectServiceError) -> AppError {
    AppError::from(DomainError::from(e))
}
impl TransferQueueAdapter for S3DownloadAdapter {
    fn cancellation(&self) -> Result<tokio_util::sync::CancellationToken, AppError> {
        self.runtime.vault.transfer_cancellation().map_err(AppError::from)
    }
    fn report_error(&self, error: &AppError) {
        if let Err(e) = self.runtime.object_events.error(error.clone()) {
            log::error!("download error event failed: {e}");
        }
    }
    fn validate(&self) -> BoxFuture<'_, Result<(), AppError>> {
        Box::pin(async {
            self.runtime
                .vault
                .require_interface_unlocked()
                .map_err(AppError::from)?;
            self.runtime.object_admit()?;
            let current = self.runtime.object_session(&self.connection).await?;
            if !Arc::ptr_eq(&current, &self.session) {
                return Err(crate::local_upload::invalid());
            }
            Ok(())
        })
    }
    fn plan(
        &self,
        selection: DownloadSelection,
    ) -> BoxFuture<'_, Result<Vec<DownloadItem>, AppError>> {
        Box::pin(async move {
            self.validate().await?;
            let (paths, single) = match selection {
                DownloadSelection::Single { path } => (vec![path], true),
                DownloadSelection::Files { paths } => (paths, false),
                DownloadSelection::Folder { path } => (vec![format!("{}/", path.trim_end_matches('/'))], false),
            };
            if paths.len() > MAX_DOWNLOAD_ITEMS { return Err(crate::local_upload::invalid()); }
            let mut result = Vec::new();
            let mut targets = std::collections::HashSet::new();
            let mut ledger = crate::cloud::object_service::BudgetLedger::default();
            let mut stat_requests = 0usize;
            for path in paths {
                self.validate().await?;
                let directory = path.ends_with('/');
                let base = path.trim_end_matches('/');
                crate::cloud::object_service::valid_relative_path(base).map_err(app)?;
                let name = base.rsplit('/').next().ok_or_else(crate::local_upload::invalid)?;
                if !directory {
                    if stat_requests >= ledger.remaining_requests() { return Err(crate::local_upload::invalid()); }
                    stat_requests += 1;
                    match self.session.store.stat(&path).await {
                        Ok(metadata) if metadata.key == path => {
                            if !targets.insert(name.to_owned()) { return Err(crate::local_upload::invalid()); }
                            result.push(crate::download::item(path.clone(), name.to_owned(), metadata.size)?);
                            continue;
                        }
                        Ok(_) => return Err(crate::local_upload::invalid()),
                        Err(crate::cloud::types::CloudError::NotFound) if !single => {},
                        Err(error) => return Err(app(error.into())),
                    }
                }
                if single { return Err(crate::local_upload::invalid()); }
                let prefix = format!("{base}/");
                let mut next = None;
                let mut seen = std::collections::HashSet::new();
                loop {
                    self.validate().await?;
                    if stat_requests >= ledger.remaining_requests() { return Err(crate::local_upload::invalid()); }
                    let page = self.session.service.browse_with_budget(&prefix, next.as_deref(), &mut ledger).await.map_err(app)?;
                    for object in page.objects {
                        let tail = object.key.strip_prefix(&prefix).ok_or_else(crate::local_upload::invalid)?;
                        if tail.is_empty() || object.key.ends_with('/') { continue; }
                        if tail.split('/').count() > MAX_DOWNLOAD_DEPTH || result.len() >= MAX_DOWNLOAD_ITEMS {
                            return Err(crate::local_upload::invalid());
                        }
                        let relative = format!("{name}/{tail}");
                        if !targets.insert(relative.clone()) { return Err(crate::local_upload::invalid()); }
                        result.push(crate::download::item(object.key, relative, object.size)?);
                    }
                    match page.next {
                        Some(token) if seen.insert(token.clone()) => next = Some(token),
                        Some(_) => return Err(crate::local_upload::invalid()),
                        None => break,
                    }
                }
            }
            Ok(result)
        })
    }
    fn execute<'a>(
        &'a self,
        item: &'a DownloadItem,
        root: &'a crate::cloud::PhysicalRoot,
        overwrite: bool,
        cancel: tokio_util::sync::CancellationToken,
        progress: Option<crate::peer::TransferProgressCallback>,
    ) -> BoxFuture<'a, Result<(), AppError>> {
        Box::pin(async move {
            let _permit = self.runtime.network.read().await;
            self.validate().await?;
            let identity = root
                .relative_target_identity(&item.relative_path)
                .map_err(app)?;
            let guard = self
                .runtime
                .objects
                .lock()
                .await
                .guard
                .clone()
                .ok_or_else(crate::local_upload::invalid)?;
            let temporary = tempfile::tempdir().map_err(crate::local_upload::io)?;
            let service = crate::cloud::ObjectService::try_new(
                Arc::clone(&self.session.store),
                guard,
                self.session.identity.clone(),
                self.session.revision.to_string(),
            )
            .map_err(app)?
            .with_physical_root(crate::cloud::PhysicalRoot::open(temporary.path()).map_err(app)?);
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(AppError::from(DomainError::TransferCanceled)),
                r = service.download_to(&item.remote_path, "payload", None, false, progress) => r.map_err(app)?,
            }
            self.validate().await?;
            if overwrite
                && root
                    .relative_target_identity(&item.relative_path)
                    .map_err(app)?
                    != identity
            {
                return Err(AppError::from(DomainError::TransferConflict {
                    path: item.relative_path.clone(),
                }));
            }
            let install_root = root.try_clone().map_err(app)?;
            let relative = item.relative_path.clone();
            tokio::task::spawn_blocking(move || {
                install_root.install_download(&relative, &temporary.path().join("payload"), overwrite, &cancel)
            }).await.map_err(|e| AppError::from(DomainError::Internal { detail: e.to_string() }))?.map_err(app)
        })
    }
}
