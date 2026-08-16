.PHONY: release clean

# Keep Cargo's artifact intact so incremental release builds remain fast, and
# install a runnable copy beside llama-cli and the model.
release:
	cargo build --release --locked
	cp target/release/whatisit ./whatisit

clean:
	cargo clean
	rm -f ./whatisit
