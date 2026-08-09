//! 本地资源接口的响应 DTO。

use serde::Serialize;

/// 图片上传响应体。
#[derive(Serialize)]
pub struct AssetUploadResponse {
    pub url: String,
}
