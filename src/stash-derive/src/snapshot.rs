// Copyright Materialize, Inc. and contributors. All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.

use proc_macro2::TokenStream;
use quote::quote;
use std::env;
use std::fs;
use std::io::Read;
use std::io::Write;
use std::path::PathBuf;

const COPYRIGHT: &str = r#"// Copyright Materialize, Inc. and contributors. All rights reserved.
//
// Use of this software is governed by the Business Source License
// included in the LICENSE file.
//
// As of the Change Date specified in that file, in accordance with
// the Business Source License, use of this software will be governed
// by the Apache License, Version 2.0.
"#;

/// Given a struct `name`, a `version` number, and a current `TokenStream` of what that struct looks
/// like, checks if a snapshot currently exists and if so, compares them. If the snapshot doesn't
/// exist, we then generate a new one.
pub fn snapshot(name: &str, version: usize, tokens: TokenStream) -> Result<(), TokenStream> {
    let crate_root = match env::var("CARGO_MANIFEST_DIR") {
        Ok(path) => PathBuf::from(path),
        Err(_) => return Err(quote! { compile_error!("Failed to get CARGO_MANIFEST_DIR") }),
    };

    // Make sure the directory to store our snapshots exists.
    let snapshot_dir = crate_root.join("stash_snapshots").join(name.to_lowercase());
    if let Err(_) = fs::create_dir_all(&snapshot_dir) {
        return Err(
            quote! { compile_error!("Failed to create stash_snapshots in the crate root") },
        );
    }

    // Either read and assert, or create a snapshot for the type.
    let snapshot_path = snapshot_dir.join(format!("v{}.snap", version));
    match fs::File::open(&snapshot_path) {
        // Snapshot does exist, assert equality.
        Ok(mut snapshot) => {
            let mut persisted = String::new();
            snapshot
                .read_to_string(&mut persisted)
                .expect("Failed to read snapshot!");

            let current = generate_snapshot(tokens.to_string());
            if persisted != current {
                eprintln!("Snapshot:\n{persisted}\n\nCurrent:\n{current}");
                let err = quote! { compile_error!("Previous snapshot changed!") };
                return Err(err);
            }
        }
        // Snapshot doesn't exist, let's write it.
        Err(_) => {
            let current = generate_snapshot(tokens.to_string());
            let mut snapshot = fs::File::options()
                .write(true)
                .create_new(true)
                .open(&snapshot_path)
                .expect("Failed to create snapshot!");
            snapshot
                .write_all(current.as_bytes())
                .expect("Failed to write snapshot!");
            snapshot.sync_all().expect("Failed to fsync snapshot!");
            // Close the file.
            drop(snapshot);
        }
    };

    Ok(())
}

fn generate_snapshot(s: impl AsRef<str>) -> String {
    let parsed = syn::parse_file(s.as_ref()).expect("Valid Rust");
    let pretty = prettyplease::unparse(&parsed);

    format!("{COPYRIGHT}\n\n{pretty}")
}
