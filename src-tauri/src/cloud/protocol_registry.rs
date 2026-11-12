use super::protocol_types::{ProtocolDescriptor, ProtocolField};

/// Advertise implemented protocols only. Provider presets are not protocols.
pub fn descriptors() -> Vec<ProtocolDescriptor> {
    let field = |key, kind, label_zh, label_en, required, secret, advanced| ProtocolField {
        key, kind, label_zh, label_en, required, secret, advanced,
    };
    let mut descriptors = vec![ProtocolDescriptor {
        id: "s3",
        name: "S3",
        config_version: 1,
        auth: "credentials",
        browse_only: false,
        fields: vec![
            field("endpoint", "url", "服务地址", "Endpoint", true, false, false),
            field("region", "string", "区域", "Region", true, false, false),
            field("bucket", "string", "存储桶", "Bucket", true, false, false),
            field("accessKey", "string", "访问密钥 ID", "Access key ID", true, true, false),
            field("secretKey", "string", "访问密钥", "Secret access key", true, true, false),
            field("sessionToken", "string", "临时令牌", "Session token", false, true, true),
            field("prefix", "string", "根前缀", "Root prefix", false, false, true),
            field("pathStyle", "boolean", "路径寻址", "Path-style addressing", false, false, true),
        ],
    }];
    // Yandex Disk is intentionally not advertised yet: its developer account is
    // still unconfirmed, so its adapter stays disabled in `official_drives`.
    for (id, name) in [
        ("google_drive", "Google Drive"),
        ("one_drive", "OneDrive"),
        ("baidu_netdisk", "百度网盘 / Baidu Netdisk"),
        ("dropbox", "Dropbox"),
    ] {
        // 百度 OAuth 的 client_id 是应用控制台的 AppKey（API Key），不是 AppID。
        let baidu = id == "baidu_netdisk";
        descriptors.push(ProtocolDescriptor {
            id, name, config_version: 1, auth: "oauth2", browse_only: id != "google_drive",
            fields: vec![
                field("clientId", "string",
                    if baidu { "AppKey（API Key，不是 AppID）" } else { "应用客户端 ID" },
                    if baidu { "AppKey (API Key, not AppID)" } else { "Application client ID" }, true, false, false),
                field("clientSecret", "string",
                    if baidu { "Secret Key" } else { "应用客户端密钥（如需要）" },
                    if baidu { "Secret Key" } else { "Application client secret (if required)" }, false, true, false),
                field("redirectUri", "url", "已注册的回调地址", "Registered redirect URI", true, false, false),
                field("rootId", "string",
                    if baidu { "根目录路径（如 /）" } else { "根文件夹 ID 或路径" },
                    if baidu { "Root folder path (for example /)" } else { "Root folder ID or path" }, true, false, false),
            ],
        });
    }
    descriptors
}

#[cfg(test)]
mod tests {
    #[test]
    fn descriptors_are_unique_and_never_advertise_unimplemented_drivers() {
        let descriptors = super::descriptors();
        assert_eq!(descriptors.len(), 5);
        assert_eq!(descriptors[0].id, "s3");
        let mut ids = std::collections::HashSet::new();
        for descriptor in &descriptors {
            assert!(ids.insert(descriptor.id));
            assert_eq!(descriptor.browse_only, descriptor.auth == "oauth2" && descriptor.id != "google_drive");
            let mut keys = std::collections::HashSet::new();
            for field in &descriptor.fields {
                assert!(keys.insert(field.key));
                assert!(!field.label_zh.is_empty() && !field.label_en.is_empty());
                if matches!(field.key, "accessKey" | "secretKey" | "sessionToken" | "clientSecret") {
                    assert!(field.secret);
                }
            }
        }
    }

    #[test]
    fn baidu_fields_name_the_oauth_app_key_explicitly() {
        let descriptors = super::descriptors();
        let baidu = descriptors.iter().find(|descriptor| descriptor.id == "baidu_netdisk").expect("baidu descriptor");
        let label = |key| baidu.fields.iter().find(|field| field.key == key).expect("baidu field");
        assert!(label("clientId").label_zh.contains("AppKey") && label("clientId").label_zh.contains("API Key"));
        assert!(label("clientId").label_en.contains("AppKey") && label("clientId").label_en.contains("API Key"));
        assert!(label("clientSecret").label_zh.contains("Secret Key"));
        assert!(label("clientSecret").label_en.contains("Secret Key"));
        let google = descriptors.iter().find(|descriptor| descriptor.id == "google_drive").expect("google descriptor");
        assert_eq!(google.fields.iter().find(|field| field.key == "clientId").expect("client id").label_zh, "应用客户端 ID");
    }
}
