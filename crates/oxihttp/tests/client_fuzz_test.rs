//! Property-based fuzz tests for the OxiHTTP client.
//!
//! Exercises the client URL parser with randomly generated strings to verify
//! that malformed or adversarial URLs never cause a panic — at most they return
//! an `Err`.

#[cfg(feature = "client")]
mod client_fuzz {
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: 100,
            max_shrink_iters: 16,
            ..ProptestConfig::default()
        })]

        /// Feeding an arbitrary string as the URL to `Client::get()` must not
        /// panic.  The only permitted outcomes are:
        ///   - `get()` returns `Err` (invalid URI rejected at the builder stage), or
        ///   - `get()` returns `Ok` and `send()` returns `Err` (connection error).
        ///
        /// In both cases the process must remain alive.
        #[test]
        fn test_malformed_url_does_not_panic(s in ".*") {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");

            rt.block_on(async {
                let client = match oxihttp::Client::builder().build() {
                    Ok(c) => c,
                    Err(_) => return,
                };
                match client.get(&s) {
                    Ok(builder) => {
                        // URI was accepted syntactically; send must still not panic.
                        let _ = builder.send().await;
                    }
                    Err(_) => {
                        // Invalid URI rejected at the builder stage — expected outcome.
                    }
                }
            });
        }
    }
}
