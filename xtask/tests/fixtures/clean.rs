// Every secret-looking field is wrapped in `Secret<…>` or marked `#[redact]`.

pub struct Session {
    pub user: String,
    pub api_key: origin_keyvault::Secret<String>,
    #[redact]
    pub token: String,
}
