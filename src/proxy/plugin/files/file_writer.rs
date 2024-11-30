//! Writing files to Pingora session.

use crate::logs::error;
use bytes::BytesMut;
use http::status::StatusCode;

use http::{header, Method};
use maud::{html, DOCTYPE};
use pingora_error::{Error, ErrorType};
use pingora_http::ResponseHeader;
use pingora_proxy::Session;
use std::cmp::min;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const BUFFER_SIZE: usize = 64 * 1024;

/// Writes a chunk of a file as a Pingora session response. The data will be passed through the
/// compression handler first in case dynamic compression is enabled.
pub(crate) async fn file_response(
    session: &mut Session,
    path: &Path,
    start: u64,
    end: u64,
) -> Result<(), Box<Error>> {
    let mut file = File::open(path).map_err(|err| {
        error!("failed opening file {path:?}: {err}");
        Error::new(ErrorType::HTTPStatus(
            StatusCode::INTERNAL_SERVER_ERROR.into(),
        ))
    })?;

    if start != 0 {
        file.seek(SeekFrom::Start(start)).map_err(|err| {
            error!("failed seeking in file {path:?}: {err}");
            Error::new(ErrorType::HTTPStatus(
                StatusCode::INTERNAL_SERVER_ERROR.into(),
            ))
        })?;
    }

    let mut remaining = (end - start + 1) as usize;
    while remaining > 0 {
        let mut buf = BytesMut::zeroed(min(remaining, BUFFER_SIZE));
        let len = file.read(buf.as_mut()).map_err(|err| {
            error!("failed reading data from {path:?}: {err}");
            Error::new(ErrorType::HTTPStatus(
                StatusCode::INTERNAL_SERVER_ERROR.into(),
            ))
        })?;

        if len == 0 {
            error!("file ended with {remaining} bytes left to be written");
            return Err(Error::new(ErrorType::ReadError));
        }

        buf.truncate(len);
        session.write_response_body(Some(buf.into()), false).await?;
        remaining -= len;
    }

    session.write_response_body(None, true).await?;

    Ok(())
}

/// Produces the text of a standard response page for the given status code.
pub fn response_text(status: StatusCode) -> String {
    let status_str = status.as_str();
    let reason = status.canonical_reason().unwrap_or("");
    html! ({
        (DOCTYPE)
        html {
            head {
                title {
                    (status_str) " " (reason)
                }
            }

            body {
                center {
                    h1 {
                        (status_str) " " (reason)
                    }
                }
            }
        }
    })
    .into()
}

async fn response(
    session: &mut Session,
    status: StatusCode,
    location: Option<&str>,
    cookie: Option<&str>,
) -> Result<(), Box<Error>> {
    let text = response_text(status);

    let mut header = ResponseHeader::build(status, Some(4))?;
    header.append_header(header::CONTENT_LENGTH, text.len().to_string())?;
    header.append_header(header::CONTENT_TYPE, "text/html;charset=utf-8")?;
    if let Some(location) = location {
        header.append_header(header::LOCATION, location)?;
    }
    if let Some(cookie) = cookie {
        header.append_header(header::SET_COOKIE, cookie)?;
    }

    let send_body = session.req_header().method != Method::HEAD;
    session
        .write_response_header(Box::new(header), !send_body)
        .await?;

    if send_body {
        session.write_response_body(Some(text.into()), true).await?;
    }

    Ok(())
}

/// Responds with a standard error page for the given status code.
pub async fn error_response(session: &mut Session, status: StatusCode) -> Result<(), Box<Error>> {
    response(session, status, None, None).await
}

/// Responds with a redirect to the given location.
pub async fn redirect_response(
    session: &mut Session,
    status: StatusCode,
    location: &str,
) -> Result<(), Box<Error>> {
    response(session, status, Some(location), None).await
}
