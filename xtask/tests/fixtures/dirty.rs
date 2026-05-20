// `api_key` is a raw `String` with `#[derive(Debug)]` on the struct -> must
// fail the lint.

#[derive(Debug)]
pub struct Session {
    pub user: String,
    pub api_key: String, // VIOLATION
}
