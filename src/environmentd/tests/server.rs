// Copyright Materialize, Inc. and contributors. All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

// BEGIN LINT CONFIG
// DO NOT EDIT. Automatically generated by bin/gen-lints.
// Have complaints about the noise? See the note in misc/python/materialize/cli/gen-lints.py first.
#![allow(clippy::style)]
#![allow(clippy::complexity)]
#![allow(clippy::large_enum_variant)]
#![allow(clippy::mutable_key_type)]
#![allow(clippy::stable_sort_primitive)]
#![allow(clippy::map_entry)]
#![allow(clippy::box_default)]
#![warn(clippy::bool_comparison)]
#![warn(clippy::clone_on_ref_ptr)]
#![warn(clippy::no_effect)]
#![warn(clippy::unnecessary_unwrap)]
#![warn(clippy::dbg_macro)]
#![warn(clippy::todo)]
#![warn(clippy::wildcard_dependencies)]
#![warn(clippy::zero_prefixed_literal)]
#![warn(clippy::borrowed_box)]
#![warn(clippy::deref_addrof)]
#![warn(clippy::double_must_use)]
#![warn(clippy::double_parens)]
#![warn(clippy::extra_unused_lifetimes)]
#![warn(clippy::needless_borrow)]
#![warn(clippy::needless_question_mark)]
#![warn(clippy::needless_return)]
#![warn(clippy::redundant_pattern)]
#![warn(clippy::redundant_slicing)]
#![warn(clippy::redundant_static_lifetimes)]
#![warn(clippy::single_component_path_imports)]
#![warn(clippy::unnecessary_cast)]
#![warn(clippy::useless_asref)]
#![warn(clippy::useless_conversion)]
#![warn(clippy::builtin_type_shadow)]
#![warn(clippy::duplicate_underscore_argument)]
#![warn(clippy::double_neg)]
#![warn(clippy::unnecessary_mut_passed)]
#![warn(clippy::wildcard_in_or_patterns)]
#![warn(clippy::collapsible_if)]
#![warn(clippy::collapsible_else_if)]
#![warn(clippy::crosspointer_transmute)]
#![warn(clippy::excessive_precision)]
#![warn(clippy::overflow_check_conditional)]
#![warn(clippy::as_conversions)]
#![warn(clippy::match_overlapping_arm)]
#![warn(clippy::zero_divided_by_zero)]
#![warn(clippy::must_use_unit)]
#![warn(clippy::suspicious_assignment_formatting)]
#![warn(clippy::suspicious_else_formatting)]
#![warn(clippy::suspicious_unary_op_formatting)]
#![warn(clippy::mut_mutex_lock)]
#![warn(clippy::print_literal)]
#![warn(clippy::same_item_push)]
#![warn(clippy::useless_format)]
#![warn(clippy::write_literal)]
#![warn(clippy::redundant_closure)]
#![warn(clippy::redundant_closure_call)]
#![warn(clippy::unnecessary_lazy_evaluations)]
#![warn(clippy::partialeq_ne_impl)]
#![warn(clippy::redundant_field_names)]
#![warn(clippy::transmutes_expressible_as_ptr_casts)]
#![warn(clippy::unused_async)]
#![warn(clippy::disallowed_methods)]
#![warn(clippy::disallowed_macros)]
#![warn(clippy::disallowed_types)]
#![warn(clippy::from_over_into)]
// END LINT CONFIG

//! Integration tests for Materialize server.

use std::fmt::Write;
use std::time::Duration;
use std::{iter, thread};

use anyhow::bail;
use chrono::{DateTime, Utc};
use http::StatusCode;
use itertools::Itertools;
use reqwest::blocking::Client;
use reqwest::Url;
use tokio_postgres::error::SqlState;
use tracing::info;
use tungstenite::error::ProtocolError;
use tungstenite::{Error, Message};

use mz_environmentd::WebSocketResponse;
use mz_ore::retry::Retry;
use mz_pgrepr::UInt8;
use mz_sql::session::user::SYSTEM_USER;

use crate::util::{PostgresErrorExt, KAFKA_ADDRS};

pub mod util;

#[test]
fn test_persistence() {
    let data_dir = tempfile::tempdir().unwrap();
    let config = util::Config::default()
        .data_directory(data_dir.path())
        .unsafe_mode();

    {
        let server = util::start_server(config.clone()).unwrap();
        let mut client = server.connect(postgres::NoTls).unwrap();
        client
            .batch_execute(&format!(
                "CREATE CONNECTION kafka_conn TO KAFKA (BROKER '{}')",
                &*KAFKA_ADDRS,
            ))
            .unwrap();
        client
            .batch_execute(
                "CREATE SOURCE src FROM KAFKA CONNECTION kafka_conn (TOPIC 'ignored') FORMAT BYTES",
            )
            .unwrap();
        client
            .batch_execute("CREATE VIEW constant AS SELECT 1")
            .unwrap();
        client.batch_execute(
            "CREATE VIEW mat (a, a_data, c, c_data) AS SELECT 'a', data, 'c' AS c, data FROM src",
        ).unwrap();
        client.batch_execute("CREATE DEFAULT INDEX ON mat").unwrap();
        client.batch_execute("CREATE DATABASE d").unwrap();
        client.batch_execute("CREATE SCHEMA d.s").unwrap();
        client
            .batch_execute("CREATE VIEW d.s.v AS SELECT 1")
            .unwrap();
    }

    let server = util::start_server(config).unwrap();
    let mut client = server.connect(postgres::NoTls).unwrap();
    assert_eq!(
        client
            .query("SHOW VIEWS", &[])
            .unwrap()
            .into_iter()
            .map(|row| row.get(0))
            .collect::<Vec<String>>(),
        &["constant", "mat"]
    );
    assert_eq!(
        client
            .query_one("SHOW INDEXES ON mat", &[])
            .unwrap()
            .get::<_, Vec<String>>("key"),
        &["a", "a_data", "c", "c_data"],
    );
    assert_eq!(
        client
            .query("SHOW VIEWS FROM d.s", &[])
            .unwrap()
            .into_iter()
            .map(|row| row.get(0))
            .collect::<Vec<String>>(),
        &["v"]
    );

    // Test that catalog recovery correctly populates `mz_objects`.
    assert_eq!(
        client
            .query(
                "SELECT id FROM mz_objects WHERE id LIKE 'u%' ORDER BY 1",
                &[]
            )
            .unwrap()
            .into_iter()
            .map(|row| row.get(0))
            .collect::<Vec<String>>(),
        vec!["u1", "u2", "u3", "u4", "u5", "u6", "u7"]
    );
}

// Test that sources and sinks require an explicit `SIZE` parameter outside of
// unsafe mode.
#[test]
fn test_source_sink_size_required() {
    let server = util::start_server(util::Config::default()).unwrap();
    let mut client = server.connect(postgres::NoTls).unwrap();

    // Sources bail without an explicit size.
    let result = client.batch_execute("CREATE SOURCE lg FROM LOAD GENERATOR COUNTER");
    assert_eq!(
        result.unwrap_err().unwrap_db_error().message(),
        "size option is required"
    );

    // Sources work with an explicit size.
    client
        .batch_execute("CREATE SOURCE lg FROM LOAD GENERATOR COUNTER WITH (SIZE '1')")
        .unwrap();

    // `ALTER SOURCE ... RESET SIZE` is banned.
    let result = client.batch_execute("ALTER SOURCE lg RESET (SIZE)");
    assert_eq!(
        result.unwrap_err().unwrap_db_error().message(),
        "size option is required"
    );

    client
        .batch_execute(&format!(
            "CREATE CONNECTION conn TO KAFKA (BROKER '{}')",
            &*KAFKA_ADDRS,
        ))
        .unwrap();

    // Sinks bail without an explicit size.
    let result = client.batch_execute("CREATE SINK snk FROM mz_sources INTO KAFKA CONNECTION conn (TOPIC 'foo') FORMAT JSON ENVELOPE DEBEZIUM");
    assert_eq!(
        result.unwrap_err().unwrap_db_error().message(),
        "size option is required"
    );

    // Sinks work with an explicit size.
    client.batch_execute("CREATE SINK snk FROM mz_sources INTO KAFKA CONNECTION conn (TOPIC 'foo') FORMAT JSON ENVELOPE DEBEZIUM WITH (SIZE '1')").unwrap();

    // `ALTER SINK ... RESET SIZE` is banned.
    let result = client.batch_execute("ALTER SINK snk RESET (SIZE)");
    assert_eq!(
        result.unwrap_err().unwrap_db_error().message(),
        "size option is required"
    );
}

// Test the POST and WS server endpoints.
#[test]
#[cfg_attr(miri, ignore)] // unsupported operation: can't call foreign function `epoll_wait` on OS `linux`
fn test_http_sql() {
    // Datadriven directives for WebSocket are "ws-text" and "ws-binary" to send
    // text or binary websocket messages that are the input. Output is
    // everything until and including the next ReadyForQuery message. An
    // optional "rows=N" argument can be given in the directive to produce
    // datadriven output after N rows. Any directive with rows=N should be the
    // final directive in a file, since it leaves the websocket in a
    // mid-statement state. A "fixtimestamp=true" argument can be given to
    // replace timestamps with "<TIMESTAMP>".
    //
    // Datadriven directive for HTTP POST is "http". Input and output are the
    // documented JSON formats.

    let fixtimestamp_re = regex::Regex::new("\\d{13}(\\.0)?").unwrap();
    let fixtimestamp_replace = "<TIMESTAMP>";

    datadriven::walk("tests/testdata/http", |f| {
        let server = util::start_server(util::Config::default()).unwrap();
        let ws_url = Url::parse(&format!(
            "ws://{}/api/experimental/sql",
            server.inner.http_local_addr()
        ))
        .unwrap();
        let http_url = Url::parse(&format!(
            "http://{}/api/sql",
            server.inner.http_local_addr()
        ))
        .unwrap();
        let (mut ws, _resp) = tungstenite::connect(ws_url).unwrap();
        util::auth_with_ws(&mut ws);

        f.run(|tc| {
            let msg = match tc.directive.as_str() {
                "ws-text" => Message::Text(tc.input.clone()),
                "ws-binary" => Message::Binary(tc.input.as_bytes().to_vec()),
                "http" => {
                    let json: serde_json::Value = serde_json::from_str(&tc.input).unwrap();
                    let res = Client::new()
                        .post(http_url.clone())
                        .json(&json)
                        .send()
                        .unwrap();
                    return format!("{}\n{}\n", res.status(), res.text().unwrap());
                }
                _ => panic!("unknown directive {}", tc.directive),
            };
            let mut rows = tc
                .args
                .get("rows")
                .map(|rows| rows.get(0).map(|row| row.parse::<usize>().unwrap()))
                .flatten();
            let fixtimestamp = tc.args.get("fixtimestamp").is_some();
            ws.write_message(msg).unwrap();
            let mut responses = String::new();
            loop {
                let resp = ws.read_message().unwrap();
                match resp {
                    Message::Text(mut msg) => {
                        if fixtimestamp {
                            msg = fixtimestamp_re
                                .replace_all(&msg, fixtimestamp_replace)
                                .into();
                        }
                        let msg: WebSocketResponse = serde_json::from_str(&msg).unwrap();
                        write!(&mut responses, "{}\n", serde_json::to_string(&msg).unwrap())
                            .unwrap();
                        match msg {
                            WebSocketResponse::ReadyForQuery(_) => return responses,
                            WebSocketResponse::Row(_) => {
                                if let Some(rows) = rows.as_mut() {
                                    *rows -= 1;
                                    if *rows == 0 {
                                        return responses;
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    Message::Ping(_) => continue,
                    _ => panic!("unexpected response: {:?}", resp),
                }
            }
        });
    });
}

// Test that the server properly handles cancellation requests.
#[test]
#[cfg_attr(miri, ignore)] // unsupported operation: can't call foreign function `epoll_wait` on OS `linux`
fn test_cancel_long_running_query() {
    let config = util::Config::default().unsafe_mode();
    let server = util::start_server(config).unwrap();

    let mut client = server.connect(postgres::NoTls).unwrap();
    let cancel_token = client.cancel_token();

    client.batch_execute("CREATE TABLE t (i INT)").unwrap();
    let (shutdown_tx, shutdown_rx) = std::sync::mpsc::channel();

    let handle = thread::spawn(move || {
        // Repeatedly attempt to abort the query because we're not sure exactly
        // when the SELECT will arrive.
        loop {
            thread::sleep(Duration::from_secs(1));
            match shutdown_rx.try_recv() {
                Ok(()) => return,
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    let _ = cancel_token.cancel_query(postgres::NoTls);
                }
                _ => panic!("unexpected"),
            }
        }
    });

    match client.simple_query("SELECT * FROM t AS OF 18446744073709551615") {
        Err(e) if e.code() == Some(&postgres::error::SqlState::QUERY_CANCELED) => {}
        Err(e) => panic!("expected error SqlState::QUERY_CANCELED, but got {:?}", e),
        Ok(_) => panic!("expected error SqlState::QUERY_CANCELED, but query succeeded"),
    }

    // Wait for the cancellation thread to stop.
    shutdown_tx.send(()).unwrap();
    handle.join().unwrap();

    client
        .simple_query("SELECT 1")
        .expect("simple query succeeds after cancellation");
}

// Test that dataflow uninstalls cancelled peeks.
#[test]
#[cfg_attr(miri, ignore)] // unsupported operation: can't call foreign function `epoll_wait` on OS `linux`
fn test_cancel_dataflow_removal() {
    let config = util::Config::default().unsafe_mode();
    let server = util::start_server(config).unwrap();

    let mut client1 = server.connect(postgres::NoTls).unwrap();
    let mut client2 = server.connect(postgres::NoTls).unwrap();
    let cancel_token = client1.cancel_token();

    client1.batch_execute("CREATE TABLE t (i INT)").unwrap();
    // No dataflows expected at startup.
    assert_eq!(
        client1
            .query_one(
                "SELECT count(*) FROM mz_internal.mz_dataflow_operators",
                &[]
            )
            .unwrap()
            .get::<_, i64>(0),
        0
    );

    thread::spawn(move || {
        // Wait until we see the expected dataflow.
        Retry::default()
            .retry(|_state| {
                let count: i64 = client2
                    .query_one(
                        "SELECT count(*) FROM mz_internal.mz_dataflow_operators",
                        &[],
                    )
                    .map_err(|_| ())
                    .unwrap()
                    .get(0);
                if count == 0 {
                    Err(())
                } else {
                    Ok(())
                }
            })
            .unwrap();
        cancel_token.cancel_query(postgres::NoTls).unwrap();
    });

    match client1.simple_query("SELECT * FROM t AS OF 9223372036854775807") {
        Err(e) if e.code() == Some(&postgres::error::SqlState::QUERY_CANCELED) => {}
        Err(e) => panic!("expected error SqlState::QUERY_CANCELED, but got {:?}", e),
        Ok(_) => panic!("expected error SqlState::QUERY_CANCELED, but query succeeded"),
    }
    // Expect the dataflows to shut down.
    Retry::default()
        .retry(|_state| {
            let count: i64 = client1
                .query_one(
                    "SELECT count(*) FROM mz_internal.mz_dataflow_operators",
                    &[],
                )
                .map_err(|_| ())
                .unwrap()
                .get(0);
            if count == 0 {
                Ok(())
            } else {
                Err(())
            }
        })
        .unwrap();
}

#[test]
fn test_storage_usage_collection_interval() {
    /// Waits for the next storage collection to occur, then returns the
    /// timestamp at which the collection occurred. The timestamp of the last
    /// collection must be provided
    fn wait_for_next_collection(
        client: &mut postgres::Client,
        last_timestamp: DateTime<Utc>,
    ) -> DateTime<Utc> {
        info!("waiting for next storage usage collection");
        let ts = Retry::default()
            .max_duration(Duration::from_secs(10))
            .retry(|_| {
                let row = client.query_one(
                    "SELECT max(collection_timestamp)
                    FROM mz_internal.mz_storage_usage_by_shard",
                    &[],
                )?;
                // mz_storage_usage_by_shard may not be populated yet, which would result in a NULL ts.
                let ts = row.try_get::<_, DateTime<Utc>>("max")?;
                if ts <= last_timestamp {
                    bail!("next collection has not yet occurred")
                }
                Ok(ts)
            })
            .unwrap();
        info!(%ts, "detected storage usage collection");
        ts
    }

    fn get_shard_id(client: &mut postgres::Client, name: &str) -> String {
        let row = Retry::default()
            .max_duration(Duration::from_secs(10))
            .retry(|_| {
                client.query_one(
                    "SELECT shard_id
                     FROM mz_internal.mz_storage_shards s
                     JOIN mz_objects o ON o.id = s.object_id
                     WHERE o.name = $1",
                    &[&name],
                )
            })
            .unwrap();
        row.get("shard_id")
    }

    fn get_storage_usage(
        client: &mut postgres::Client,
        shard_id: &str,
        collection_timestamp: DateTime<Utc>,
    ) -> u64 {
        let row = Retry::default()
            .max_duration(Duration::from_secs(10))
            .retry(|_| {
                client.query_one(
                    "SELECT coalesce(sum(size_bytes), 0)::uint8 AS size
                     FROM mz_internal.mz_storage_usage_by_shard
                     WHERE shard_id = $1 AND collection_timestamp = $2",
                    &[&shard_id, &collection_timestamp],
                )
            })
            .unwrap();
        row.get::<_, UInt8>("size").0
    }

    mz_ore::test::init_logging();

    let config =
        util::Config::default().with_storage_usage_collection_interval(Duration::from_secs(1));
    let server = util::start_server(config).unwrap();
    let mut client = server.connect(postgres::NoTls).unwrap();

    // Wait for the initial storage usage collection to occur.
    let timestamp = wait_for_next_collection(&mut client, DateTime::<Utc>::MIN_UTC);

    // Create a table with no data.
    client
        .batch_execute("CREATE TABLE usage_test (a int)")
        .unwrap();
    let shard_id = get_shard_id(&mut client, "usage_test");
    info!(%shard_id, "created table");

    // Test that the storage usage for the table was zero before it was
    // created.
    let pre_create_storage_usage = get_storage_usage(&mut client, &shard_id, timestamp);
    info!(%pre_create_storage_usage);
    assert_eq!(pre_create_storage_usage, 0);

    // Test that the storage usage for the table is nonzero after it is created
    // (there is some overhead even for empty tables). We wait out two storage
    // collection intervals (here and below) because the next storage collection
    // may have been concurrent with the previous operation.
    let timestamp = wait_for_next_collection(&mut client, timestamp);
    let timestamp = wait_for_next_collection(&mut client, timestamp);
    let post_create_storage_usage = get_storage_usage(&mut client, &shard_id, timestamp);
    info!(%post_create_storage_usage);
    assert!(post_create_storage_usage > 0);

    // Insert some data into the table.
    for _ in 0..3 {
        client
            .batch_execute("INSERT INTO usage_test VALUES (1)")
            .unwrap();
    }

    // Test that the storage usage for the table is larger than it was before.
    let timestamp = wait_for_next_collection(&mut client, timestamp);
    let timestamp = wait_for_next_collection(&mut client, timestamp);
    let after_insert_storage_usage = get_storage_usage(&mut client, &shard_id, timestamp);
    info!(%after_insert_storage_usage);
    assert!(after_insert_storage_usage > post_create_storage_usage);

    // Drop the table.
    client.batch_execute("DROP TABLE usage_test").unwrap();

    // Test that the storage usage is reported as zero.
    let timestamp = wait_for_next_collection(&mut client, timestamp);
    let timestamp = wait_for_next_collection(&mut client, timestamp);
    let after_drop_storage_usage = get_storage_usage(&mut client, &shard_id, timestamp);
    info!(%after_drop_storage_usage);
    assert_eq!(after_drop_storage_usage, 0);
}

#[test]
fn test_storage_usage_updates_between_restarts() {
    let data_dir = tempfile::tempdir().unwrap();
    let storage_usage_collection_interval = Duration::from_secs(3);
    let config = util::Config::default()
        .with_storage_usage_collection_interval(storage_usage_collection_interval)
        .data_directory(data_dir.path());

    // Wait for initial storage usage collection.
    let initial_timestamp: f64 = {
        let server = util::start_server(config.clone()).unwrap();
        let mut client = server.connect(postgres::NoTls).unwrap();
        // Retry because it may take some time for the initial snapshot to be taken.
        Retry::default().max_duration(Duration::from_secs(60)).retry(|_| {
            client
                    .query_one(
                        "SELECT EXTRACT(EPOCH FROM MAX(collection_timestamp))::float8 FROM mz_catalog.mz_storage_usage;",
                        &[],
                    )
                    .map_err(|e| e.to_string()).unwrap()
                    .try_get::<_, f64>(0)
                    .map_err(|e| e.to_string())
        }).unwrap()
    };

    std::thread::sleep(storage_usage_collection_interval);

    // Another storage usage collection should be scheduled immediately.
    {
        let server = util::start_server(config).unwrap();
        let mut client = server.connect(postgres::NoTls).unwrap();

        // Retry until storage usage is updated.
        Retry::default().max_duration(Duration::from_secs(60)).retry(|_| {
            let updated_timestamp = client
                .query_one("SELECT EXTRACT(EPOCH FROM MAX(collection_timestamp))::float8 FROM mz_catalog.mz_storage_usage;", &[])
                .map_err(|e| e.to_string()).unwrap()
                .try_get::<_, f64>(0)
                .map_err(|e| e.to_string()).unwrap();

            if updated_timestamp > initial_timestamp {
                Ok(())
            } else {
                Err(format!("updated storage collection timestamp {updated_timestamp} is not greater than initial timestamp {initial_timestamp}"))
            }
        }).unwrap();
    }
}

#[test]
fn test_storage_usage_doesnt_update_between_restarts() {
    let data_dir = tempfile::tempdir().unwrap();
    let storage_usage_collection_interval = Duration::from_secs(10);
    let config = util::Config::default()
        .with_storage_usage_collection_interval(storage_usage_collection_interval)
        .data_directory(data_dir.path());

    // Wait for initial storage usage collection.
    let initial_timestamp: f64 = {
        let server = util::start_server(config.clone()).unwrap();
        let mut client = server.connect(postgres::NoTls).unwrap();
        // Retry because it may take some time for the initial snapshot to be taken.
        Retry::default().max_duration(Duration::from_secs(60)).retry(|_| {
            client
                    .query_one(
                        "SELECT EXTRACT(EPOCH FROM MAX(collection_timestamp))::float8 FROM mz_catalog.mz_storage_usage;",
                        &[],
                    )
                    .map_err(|e| e.to_string()).unwrap()
                    .try_get::<_, f64>(0)
                    .map_err(|e| e.to_string())
        }).unwrap()
    };

    // Another storage usage collection should not be scheduled immediately.
    {
        // Give plenty of time so we don't accidentally do another collection if this test is slow.
        let storage_usage_collection_interval = Duration::from_secs(60 * 10);
        let config =
            config.with_storage_usage_collection_interval(storage_usage_collection_interval);
        let server = util::start_server(config).unwrap();
        let mut client = server.connect(postgres::NoTls).unwrap();

        let updated_timestamp = client
            .query_one(
                "SELECT EXTRACT(EPOCH FROM MAX(collection_timestamp))::float8 FROM mz_catalog.mz_storage_usage;",
                &[],
            ).unwrap().get::<_, f64>(0);

        assert_eq!(initial_timestamp, updated_timestamp);
    }
}

#[test]
fn test_storage_usage_collection_interval_timestamps() {
    let config =
        util::Config::default().with_storage_usage_collection_interval(Duration::from_secs(5));
    let server = util::start_server(config).unwrap();
    let mut client = server.connect(postgres::NoTls).unwrap();

    // Retry because it may take some time for the initial snapshot to be taken.
    Retry::default().max_duration(Duration::from_secs(10)).retry(|_| {
        let rows = client
            .query(
                "SELECT collection_timestamp, SUM(size_bytes)::int8 FROM mz_catalog.mz_storage_usage GROUP BY collection_timestamp ORDER BY collection_timestamp;",
                &[],
            )
            .map_err(|e| e.to_string()).unwrap();
        if rows.len() == 1 {
            Ok(())
        } else {
            Err(format!("expected a single timestamp, instead found {}", rows.len()))
        }
    }).unwrap();
}

#[test]
#[ignore] // Flaking often, tracked in #18108
fn test_old_storage_usage_records_are_reaped_on_restart() {
    mz_ore::test::init_logging();

    let data_dir = tempfile::tempdir().unwrap();
    let collection_interval = Duration::from_secs(1);
    let retention_period = Duration::from_millis(1100);
    let config = util::Config::default()
        .with_storage_usage_collection_interval(collection_interval)
        .with_storage_usage_retention_period(retention_period)
        .data_directory(data_dir.path());

    {
        let server = util::start_server(config.clone()).unwrap();
        let mut client = server.connect(postgres::NoTls).unwrap();
        // Create a table with no data, which should have some overhead and therefore some storage usage
        client
            .batch_execute("CREATE TABLE usage_test (a int)")
            .unwrap();

        // Wait for initial storage usage collection, to be sure records are present.
        let initial_timestamp = Retry::default().max_duration(Duration::from_secs(5)).retry(|_| {
            client
                    .query_one(
                        "SELECT EXTRACT(EPOCH FROM MAX(collection_timestamp))::integer FROM mz_internal.mz_storage_usage_by_shard;",
                        &[],
                    )
                    .map_err(|e| e.to_string()).unwrap()
                    .try_get::<_, i32>(0)
                    .map_err(|e| e.to_string())
        }).expect("Could not fetch initial timestamp");

        let initial_server_usage_records = client
            .query_one(
                "SELECT COUNT(*)::integer AS number
                     FROM mz_internal.mz_storage_usage_by_shard",
                &[],
            )
            .unwrap()
            .try_get::<_, i32>(0)
            .expect("Could not get initial count of records");

        info!(%initial_timestamp, %initial_server_usage_records);
        assert!(
            initial_server_usage_records >= 1,
            "No initial server usage records!"
        );
    };

    // Wait, start a new server, and assert that the previous storage records have been reaped
    std::thread::sleep(retention_period);

    {
        let server = util::start_server(config).unwrap();
        let mut client = server.connect(postgres::NoTls).unwrap();

        let subsequent_server_usage_records = client
            .query_one(
                "SELECT COUNT(*)::integer AS number
                     FROM mz_internal.mz_storage_usage_by_shard",
                &[],
            )
            .unwrap()
            .try_get::<_, i32>(0)
            .expect("Could not get subsequent count of records");

        info!(%subsequent_server_usage_records);
        assert_eq!(
            subsequent_server_usage_records, 0,
            "Records were not reaped!"
        );
    };
}

#[test]
#[cfg_attr(miri, ignore)] // unsupported operation: can't call foreign function `epoll_wait` on OS `linux`
fn test_default_cluster_sizes() {
    let config = util::Config::default()
        .with_builtin_cluster_replica_size("1".to_string())
        .with_default_cluster_replica_size("2".to_string());
    let server = util::start_server(config).unwrap();
    let mut client = server.connect(postgres::NoTls).unwrap();

    let builtin_size: String = client
        .query(
            "SELECT size FROM (SHOW CLUSTER REPLICAS WHERE cluster LIKE 'mz_%')",
            &[],
        )
        .unwrap()
        .get(0)
        .unwrap()
        .get(0);
    assert_eq!(builtin_size, "1");

    let builtin_size: String = client
        .query(
            "SELECT size FROM (SHOW CLUSTER REPLICAS WHERE cluster = 'default')",
            &[],
        )
        .unwrap()
        .get(0)
        .unwrap()
        .get(0);
    assert_eq!(builtin_size, "2");
}

#[test]
#[cfg_attr(miri, ignore)] // unsupported operation: can't call foreign function `epoll_wait` on OS `linux`
fn test_max_request_size() {
    let statement = "SELECT $1::text";
    let statement_size = statement.bytes().count();
    let server = util::start_server(util::Config::default()).unwrap();

    // pgwire
    {
        let param_size = mz_pgwire::MAX_REQUEST_SIZE - statement_size + 1;
        let param = std::iter::repeat("1").take(param_size).join("");
        let mut client = server.connect(postgres::NoTls).unwrap();

        // The specific error isn't forwarded to the client, the connection is just closed.
        assert!(client.query(statement, &[&param]).is_err());
        assert!(client.is_valid(Duration::from_secs(2)).is_err());
    }

    // http
    {
        let param_size = mz_environmentd::http::MAX_REQUEST_SIZE - statement_size + 1;
        let param = std::iter::repeat("1").take(param_size).join("");
        let http_url = Url::parse(&format!(
            "http://{}/api/sql",
            server.inner.http_local_addr()
        ))
        .unwrap();
        let json = format!("{{\"queries\":[{{\"query\":\"{statement}\",\"params\":[{param}]}}]}}");
        let json: serde_json::Value = serde_json::from_str(&json).unwrap();
        let res = Client::new().post(http_url).json(&json).send().unwrap();
        assert_eq!(res.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    // ws
    {
        let param_size = mz_environmentd::http::MAX_REQUEST_SIZE - statement_size + 1;
        let param = std::iter::repeat("1").take(param_size).join("");
        let ws_url = Url::parse(&format!(
            "ws://{}/api/experimental/sql",
            server.inner.http_local_addr()
        ))
        .unwrap();
        let (mut ws, _resp) = tungstenite::connect(ws_url).unwrap();
        util::auth_with_ws(&mut ws);
        let json =
            format!("{{\"queries\":[{{\"query\":\"{statement}\",\"params\":[\"{param}\"]}}]}}");
        let json: serde_json::Value = serde_json::from_str(&json).unwrap();
        ws.write_message(Message::Text(json.to_string())).unwrap();

        // The specific error isn't forwarded to the client, the connection is just closed.
        let err = ws.read_message().unwrap_err();
        assert!(matches!(
            err,
            Error::Protocol(ProtocolError::ResetWithoutClosingHandshake)
        ));
    }
}

#[test]
#[cfg_attr(miri, ignore)] // too slow
fn test_max_statement_batch_size() {
    let statement = "SELECT 1;";
    let statement_size = statement.bytes().count();
    let max_statement_size = mz_sql_parser::parser::MAX_STATEMENT_BATCH_SIZE;
    let max_statement_count = max_statement_size / statement_size + 1;
    let statements = iter::repeat(statement).take(max_statement_count).join("");
    let server = util::start_server(util::Config::default()).unwrap();

    // pgwire
    {
        let mut client = server.connect(postgres::NoTls).unwrap();

        let err = client
            .batch_execute(&statements)
            .expect_err("statement should be too large")
            .unwrap_db_error();
        assert_eq!(&SqlState::PROGRAM_LIMIT_EXCEEDED, err.code());
        assert!(
            err.message().contains("statement batch size cannot exceed"),
            "error should indicate that the statement was too large: {}",
            err.message()
        );
    }

    // http
    {
        let http_url = Url::parse(&format!(
            "http://{}/api/sql",
            server.inner.http_local_addr()
        ))
        .unwrap();
        let json = format!("{{\"query\":\"{statements}\"}}");
        let json: serde_json::Value = serde_json::from_str(&json).unwrap();

        let res = Client::new().post(http_url).json(&json).send().unwrap();
        assert!(
            res.status().is_client_error(),
            "statement should result in an error: {res:?}"
        );
        let text = res.text().unwrap();
        assert!(
            text.contains("statement batch size cannot exceed"),
            "error should indicate that the statement was too large: {}",
            text
        );
    }

    // ws
    {
        let ws_url = Url::parse(&format!(
            "ws://{}/api/experimental/sql",
            server.inner.http_local_addr()
        ))
        .unwrap();
        let (mut ws, _resp) = tungstenite::connect(ws_url).unwrap();
        util::auth_with_ws(&mut ws);
        let json = format!("{{\"query\":\"{statements}\"}}");
        let json: serde_json::Value = serde_json::from_str(&json).unwrap();
        ws.write_message(Message::Text(json.to_string())).unwrap();

        let msg = ws.read_message().unwrap();
        let msg = msg.into_text().expect("response should be text");
        let msg: WebSocketResponse = serde_json::from_str(&msg).unwrap();
        match msg {
            WebSocketResponse::Error(err) => assert!(
                err.contains("statement batch size cannot exceed"),
                "error should indicate that the statement was too large: {}",
                err
            ),
            msg @ WebSocketResponse::ReadyForQuery(_)
            | msg @ WebSocketResponse::Notice(_)
            | msg @ WebSocketResponse::Rows(_)
            | msg @ WebSocketResponse::Row(_)
            | msg @ WebSocketResponse::CommandComplete(_) => {
                panic!("response should be error: {msg:?}")
            }
        }
    }
}

#[test]
#[cfg_attr(miri, ignore)] // unsupported operation: can't call foreign function `epoll_wait` on OS `linux`
fn test_mz_system_user_admin() {
    let config = util::Config::default();
    let server = util::start_server(config).unwrap();
    let mut client = server
        .pg_config_internal()
        .user(&SYSTEM_USER.name)
        .connect(postgres::NoTls)
        .unwrap();
    assert_eq!(
        "on".to_string(),
        client
            .query_one("SHOW is_superuser;", &[])
            .unwrap()
            .get::<_, String>(0)
    );
}
