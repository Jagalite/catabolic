// SPDX-FileCopyrightText: 2026 The Catabolic Contributors
// SPDX-License-Identifier: MIT
use crate::{
    problem::{Problem, Result},
    routes::{self, Input},
    server::App,
};
use axum::{
    body::{Body, Bytes},
    http::Response,
};
use catabolic_store::{Store, access::Access, api_events};
use serde_json::json;
use std::sync::Arc;
pub async fn response(app: Arc<App>, input: Input) -> Result<Response<Body>> {
    let secret = input.bearer()?.to_owned();
    let initial_cursor = input
        .cursor()
        .filter(|v| !v.is_empty())
        .or(input.header("last-event-id"))
        .map(str::to_owned);
    let initialize = app.clone();
    let (principal, database) = tokio::task::spawn_blocking(move || -> Result<_> {
        let access = routes::session(&initialize, &input, true)?;
        api_events::validate_cursor(
            &access,
            input
                .cursor()
                .filter(|v| !v.is_empty())
                .or(input.header("last-event-id")),
        )?;
        Ok((
            access.principal().to_owned(),
            access.store().database_id.clone(),
        ))
    })
    .await
    .map_err(|_| Problem::new("catalog_error", 500))??;
    let guard = app.limits.admit(&principal)?;
    let (sender, receiver) =
        tokio::sync::mpsc::channel::<std::result::Result<Bytes, std::io::Error>>(1);
    tokio::spawn(async move {
        let mut cursor = initial_cursor;
        for _ in 0..60 {
            // Reserve before refreshing the token and principal-scoped event
            // page; a slow reader must not send a cached authorization result.
            let Ok(permit) = sender.reserve().await else {
                return;
            };
            let path = app.database.clone();
            let secret = secret.clone();
            let previous = cursor.clone();
            let expected = database.clone();
            let result = tokio::task::spawn_blocking(move || -> Result<_> {
                let store = Store::open(&path, true, false)?;
                if store.database_id != expected {
                    return Err(Problem::new("unauthorized", 401));
                }
                let mut access = Access::authenticate(store, &secret)?;
                Ok(api_events::page(&mut access, previous.as_deref())?)
            })
            .await;
            match result {
                Ok(Ok(value)) => {
                    let next = value["cursor"].as_str().unwrap().to_owned();
                    let data = format!(
                        "id: {next}\nevent: catalog\ndata: {}\n\n",
                        catabolic_core::encode(&value["events"])
                    );
                    cursor = Some(next);
                    permit.send(Ok(Bytes::from(data)));
                }
                Ok(Err(e)) if e.code == "catalog_busy" => {
                    permit.send(Ok(Bytes::from_static(b": catalog-busy\n\n")));
                }
                Ok(Err(e)) => {
                    let data = format!(
                        "event: resync-required\ndata: {}\n\n",
                        catabolic_core::encode(&json!({"code":e.code}))
                    );
                    permit.send(Ok(Bytes::from(data)));
                    return;
                }
                Err(_) => return,
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    });
    let stream = futures::stream::unfold((receiver, guard), |(mut receiver, guard)| async move {
        receiver
            .recv()
            .await
            .map(|value| (value, (receiver, guard)))
    });
    Ok(Response::builder()
        .header("Content-Type", "text/event-stream; charset=utf-8")
        .header("Cache-Control", "no-store")
        .body(Body::from_stream(stream))
        .unwrap())
}
