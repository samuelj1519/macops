use serde::Serialize;
use std::fmt;

#[derive(Debug, Serialize)]
pub struct Error {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os_code: Option<i32>,
    pub code: String,
    pub message: String,
    #[serde(skip)]
    pub exit: u8,
    #[serde(skip)]
    pub(crate) guidance: Option<Guidance>,
}

#[derive(Debug)]
pub(crate) struct Guidance {
    pub problem: String,
    pub usage: String,
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            os_code: None,
            code: code.into(),
            message: message.into(),
            exit: 1,
            guidance: None,
        }
    }

    pub fn attention(code: &str, message: impl Into<String>) -> Self {
        Self {
            os_code: None,
            code: code.into(),
            message: message.into(),
            exit: 3,
            guidance: None,
        }
    }
    pub(crate) fn with_guidance(
        mut self,
        problem: impl Into<String>,
        usage: impl Into<String>,
    ) -> Self {
        self.guidance = Some(Guidance {
            problem: problem.into(),
            usage: usage.into(),
        });
        self
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        let os_code = e.raw_os_error();
        Self {
            os_code,
            ..Self::new("io", e.to_string())
        }
    }
}

impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Self::new(
            "invalid_json",
            format!(
                "JSON 解析错误（{:?}，行 {}，列 {}）",
                e.classify(),
                e.line(),
                e.column()
            ),
        )
    }
}
