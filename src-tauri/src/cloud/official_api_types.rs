use serde::Serialize;
use zeroize::Zeroizing;

pub struct OfficialApi {
    #[cfg(test)]
    pub(crate) fixture: Option<String>,
    pub(crate) client: reqwest::Client,
    pub(crate) provider: super::oauth_types::OfficialProvider,
    pub(crate) access_token: Zeroizing<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OfficialEntry {
    pub id: String,
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub content_type: Option<String>,
    pub revision: Option<String>,
    pub online_preview: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OfficialPage {
    pub entries: Vec<OfficialEntry>,
    pub next: Option<String>,
    pub complete: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum OfficialApiError {
    #[error("网盘参数无效 / Invalid drive request")]
    InvalidInput,
    #[error("网盘响应无效 / Invalid drive response")]
    InvalidResponse,
    #[error("网盘请求失败 / Drive request failed")]
    Transport(#[source] reqwest::Error),
    #[error("网盘返回 HTTP {status}：{message}")]
    Http { status: u16, message: String },
    #[error("网盘接口错误 {code}：{message}")]
    Provider { code: String, message: String },
}
