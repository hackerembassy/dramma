set -e
# THIS SHIT WAS VIBECODED. FUCK NIXOS, FUCK DEBIAN

# Build a statically-linked portable binary using musl
# This binary will work on the Debian dramma machine without Nix dependencies

echo "🔨 Building portable static binary with musl..."

# Add musl target if not already installed
if ! rustup target list --installed | grep -q "x86_64-unknown-linux-musl"; then
    echo "📦 Installing musl target..."
    rustup target add x86_64-unknown-linux-musl
fi

# Build with musl for static linking
cargo build --release --target x86_64-unknown-linux-musl

echo "✅ Build complete!"
echo "Binary location: target/x86_64-unknown-linux-musl/release/dramma"

# Copy to standard location for deploy.sh
mkdir -p target/release
cp target/x86_64-unknown-linux-musl/release/dramma target/release/dramma

echo "📋 Binary info:"
file target/release/dramma
ls -lh target/release/dramma
