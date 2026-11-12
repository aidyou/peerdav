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
        descriptors.push(ProtocolDescriptor {
            id, name, config_version: 1, auth: "oauth2", browse_only: id != "google_drive",
            fields: vec![
                field("clientId", "string", "应用客户端 ID", "Application client ID", true, false, false),
                field("clientSecret", "string", "应用客户端密钥（如需要）", "Application client secret (if required)", false, true, false),
                field("redirectUri", "url", "已注册的回调地址", "Registered redirect URI", true, false, false),
                field("rootId", "string", "根文件夹 ID 或路径", "Root folder ID or path", true, false, false),
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
}
