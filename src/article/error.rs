use rocket::http::uri::error::PathError;
use rocket::http::Status;
use rocket::response::Responder;
use rocket::tokio::task::JoinError;
use std::string::FromUtf8Error;

#[derive(thiserror::Error, Debug)]
pub enum ArticleError {
    #[error("Malformed path")]
    MalformedPath(PathError),
    #[error("Not markdown")]
    NotMarkdown,
    #[error("IO error")]
    IoError(#[from] std::io::Error),
    #[error("No article")]
    NoArticle,
    #[error("Join error")]
    JoinError(#[from] JoinError),
    #[error("UTF-8 error")]
    Utf8Error(#[from] FromUtf8Error),
    #[error("Pandoc failed")]
    PandocFailed(String),
    #[error("JSON error")]
    JsonError(#[from] serde_json::Error),
    #[error("This article isn't ready to be published yet")]
    NotForPublication,
}

impl<'r, 'o: 'r> Responder<'r, 'o> for ArticleError {
    fn respond_to(self, request: &'r rocket::Request<'_>) -> rocket::response::Result<'o> {
        match self {
            ArticleError::MalformedPath(_) => Status::BadRequest.respond_to(request),
            ArticleError::NoArticle
            | ArticleError::NotMarkdown
            | ArticleError::NotForPublication => Status::NotFound.respond_to(request),
            ArticleError::IoError(_)
            | ArticleError::JoinError(_)
            | ArticleError::Utf8Error(_)
            | ArticleError::PandocFailed(_)
            | ArticleError::JsonError(_) => Status::InternalServerError.respond_to(request),
        }
    }
}
