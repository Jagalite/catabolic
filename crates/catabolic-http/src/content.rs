// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
//! Backpressure precedes fresh authorization and each bounded descriptor read.
use crate::{
    limits::StreamGuard,
    problem::{Problem, Result},
    routes::Input,
    server::App,
};
use axum::{
    body::{Body, Bytes},
    http::Response,
};
use catabolic_store::{Error, Store, access::Access, content_access as content, migration::rows};
use std::{cell::Cell, sync::Arc};
struct Prepared {
    status: u16,
    headers: Vec<(String, String)>,
    start: u64,
    length: u64,
    slot: Option<StreamGuard>,
    head: bool,
}
pub async fn response(app: Arc<App>, input: Input) -> Result<Response<Body>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let handle = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        let mut reply = Some(tx);
        let result = run(&app, &input, &handle, &mut reply);
        if let Err(error) = result
            && let Some(reply) = reply.take()
        {
            let _ = reply.send(Err(error));
        }
    });
    rx.await.map_err(|_| Problem::new("catalog_error", 500))?
}
type Reply = Option<tokio::sync::oneshot::Sender<Result<Response<Body>>>>;
fn run(app: &App, input: &Input, handle: &tokio::runtime::Handle, reply: &mut Reply) -> Result<()> {
    if input.operation.ends_with("playback_segment") {
        return playback_segment(app, input, handle, reply);
    }
    let revision = input
        .query
        .get("revision")
        .ok_or_else(|| Problem::new("invalid_request", 422))?;
    let ticket = input.query.get("ticket").filter(|v| !v.is_empty());
    let store = Store::open(&app.database, ticket.is_some(), false)?;
    let rendition = input.operation.ends_with("rendition_content");
    let (access, ticket_id) = if let Some(secret) = ticket {
        let file = if rendition {
            rows(
                &store.db,
                "SELECT file_id FROM api_tickets WHERE id=?",
                &[secret.split('.').next().unwrap_or("").to_owned().into()],
            )?
            .first()
            .and_then(|r| r["file_id"].as_str())
            .ok_or_else(|| Problem::new("unauthorized", 401))?
            .to_owned()
        } else {
            input.id().to_owned()
        };
        let (a, t) = content::authenticate_ticket(store, secret, &file, revision)?;
        (a, Some(t))
    } else {
        (Access::authenticate(store, input.bearer()?)?, None)
    };
    app.limits.request(access.principal())?;
    let id = if rendition {
        rows(
            &access.store().db,
            "SELECT file_id FROM media_outputs WHERE id=? AND profile=?",
            &[
                input.id().to_owned().into(),
                access.profile().to_owned().into(),
            ],
        )?
        .first()
        .and_then(|r| r["file_id"].as_str())
        .ok_or_else(|| Problem::new("not_found", 404))?
        .to_owned()
    } else {
        input.id().to_owned()
    };
    if let Some(ticket) = &ticket_id
        && !access
            .store()
            .db
            .prepare("SELECT 1 FROM api_tickets WHERE id=? AND file_id=? AND revision=?")?
            .exists([ticket, &id, revision])?
    {
        return Err(Problem::new("not_found", 404));
    }
    let size = Cell::new(None);
    let result = content::with_transfer(
        access,
        &id,
        revision,
        ticket_id.as_deref(),
        |access, stat, row| {
            size.set(Some(stat.len()));
            let etag = format!("W/\"{revision}\"");
            let mime = catabolic_store::mime::guess(row["path"].as_str().unwrap_or(""))
                .unwrap_or_else(|| "application/octet-stream".into());
            let mut headers = vec![
                ("ETag".into(), etag.clone()),
                ("Accept-Ranges".into(), "bytes".into()),
                ("Cache-Control".into(), "private, no-store".into()),
                ("X-Content-Type-Options".into(), "nosniff".into()),
                ("Referrer-Policy".into(), "no-referrer".into()),
                (
                    "Content-Type".into(),
                    if mime.starts_with("text/") {
                        format!("{mime}; charset=utf-8")
                    } else {
                        mime.clone()
                    },
                ),
            ];
            if ["text/html", "image/svg+xml", "application/xhtml+xml"].contains(&mime.as_str()) {
                headers.push(("Content-Disposition".into(), "attachment".into()))
            }
            if input.header("if-match").is_some_and(|v| v != "*") {
                return Err(Error("precondition_failed".into()));
            }
            if input
                .header("if-none-match")
                .unwrap_or("")
                .split(',')
                .any(|v| {
                    let v = v.trim().strip_prefix("W/").unwrap_or(v.trim());
                    v == "*" || v == etag.strip_prefix("W/").unwrap()
                })
            {
                headers.retain(|(name, _)| name != "Content-Type");
                return Ok(Prepared {
                    status: 304,
                    headers,
                    start: 0,
                    length: 0,
                    slot: None,
                    head: true,
                });
            }
            let range =
                if input.method == "GET" && input.header("if-range").is_none_or(str::is_empty) {
                    input.header("range")
                } else {
                    None
                };
            let (start, length, partial) = content::byte_range(range, stat.len())?;
            headers.push(("Content-Length".into(), length.to_string()));
            if partial {
                headers.push((
                    "Content-Range".into(),
                    format!(
                        "bytes {start}-{}/{total}",
                        start + length - 1,
                        total = stat.len()
                    ),
                ))
            }
            let head = input.method == "HEAD";
            let slot = if head {
                None
            } else {
                Some(
                    app.limits
                        .admit(access.principal())
                        .map_err(|e| Error(e.code))?,
                )
            };
            if !head && let Some(ticket) = &ticket_id {
                content::debit_ticket(access, ticket, length)?
            }
            Ok(Prepared {
                status: if partial { 206 } else { 200 },
                headers,
                start,
                length,
                slot,
                head,
            })
        },
        |file, stat, evidence, prepared| {
            let mut response = Response::builder().status(prepared.status);
            for (k, v) in prepared.headers {
                response = response.header(k, v)
            }
            if prepared.head {
                if let Some(reply) = reply.take() {
                    let _ = reply.send(Ok(response.body(Body::empty()).unwrap()));
                }
                return Ok(());
            }
            // A complete body of a ready generated output is hashed while it
            // streams; its final chunk is withheld unless the digest matches.
            let mut chunks = evidence.body(file, stat, prepared.start, prepared.length)?;
            let (sender, receiver) =
                tokio::sync::mpsc::channel::<std::result::Result<Bytes, std::io::Error>>(1);
            let body = Body::from_stream(futures::stream::unfold(
                (receiver, prepared.slot),
                |(mut receiver, guard)| async move {
                    receiver
                        .recv()
                        .await
                        .map(|value| (value, (receiver, guard)))
                },
            ));
            if reply
                .take()
                .is_none_or(|reply| reply.send(Ok(response.body(body).unwrap())).is_err())
            {
                return Err(Error("transfer_disconnected".into()));
            }
            while chunks.remaining() > 0 {
                // Reserve before opening authorization so network backpressure owns no Store.
                let permit = handle
                    .block_on(sender.reserve())
                    .map_err(|_| Error("transfer_disconnected".into()))?;
                match chunks.next_chunk() {
                    Ok(bytes) => permit.send(Ok(Bytes::from(bytes))),
                    Err(error) => {
                        // An error item aborts the response before its declared
                        // length, so the client never sees a completed body.
                        permit.send(Err(std::io::Error::other(
                            "content authorization or revision changed",
                        )));
                        return Err(error);
                    }
                }
            }
            Ok(())
        },
    );
    result.map_err(|e| {
        let mut problem: Problem = e.into();
        if problem.status == 416
            && let Some(size) = size.get()
        {
            problem
                .headers
                .push(("Content-Range".into(), format!("bytes */{size}")))
        }
        problem
    })
}
fn playback_segment(
    app: &App,
    input: &Input,
    handle: &tokio::runtime::Handle,
    reply: &mut Reply,
) -> Result<()> {
    let mut access = crate::routes::session(app, input, false)?;
    let principal = access.principal().to_owned();
    let tools = access.detached(|| catabolic_runtime::rendering::tools(false))?;
    let transfer = catabolic_store::playback::open_transfer(
        access,
        input.id(),
        input
            .params
            .get("segment")
            .map(String::as_str)
            .unwrap_or(""),
        &tools,
    )?;
    if input.header("range").is_some_and(|v| !v.is_empty()) {
        return Err(Problem::new("playback_segment_range_unsupported", 416));
    }
    let length = transfer.length();
    let response = Response::builder()
        .header("Content-Type", "video/mp2t")
        .header("Cache-Control", "private, no-store")
        .header("X-Content-Type-Options", "nosniff")
        .header("Content-Length", length);
    if input.method == "HEAD" {
        if let Some(reply) = reply.take() {
            let _ = reply.send(Ok(response.body(Body::empty()).unwrap()));
        }
        return Ok(());
    }
    let guard = app.limits.admit(&principal)?;
    let (sender, receiver) =
        tokio::sync::mpsc::channel::<std::result::Result<Bytes, std::io::Error>>(1);
    let body = Body::from_stream(futures::stream::unfold(
        (receiver, guard),
        |(mut receiver, guard)| async move {
            receiver
                .recv()
                .await
                .map(|value| (value, (receiver, guard)))
        },
    ));
    if reply
        .take()
        .is_none_or(|reply| reply.send(Ok(response.body(body).unwrap())).is_err())
    {
        return Ok(());
    }
    let mut offset = 0;
    while offset < length {
        let permit = handle
            .block_on(sender.reserve())
            .map_err(|_| Problem::new("transfer_disconnected", 409))?;
        match transfer.read_chunk(offset, (length - offset).min(65536) as usize) {
            Ok(bytes) => {
                offset += bytes.len() as u64;
                permit.send(Ok(Bytes::from(bytes)));
            }
            Err(error) => {
                permit.send(Err(std::io::Error::other(
                    "playback authorization or revision changed",
                )));
                return Err(error.into());
            }
        }
    }
    Ok(())
}
