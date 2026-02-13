set -e
# THIS SHIT WAS VIBECODED. FUCK NIXOS, FUCK DEBIAN

# Configuration
DRAMMA_HOST="dramma.lan"
DRAMMA_USER="dramma"
REMOTE_DIR="/home/dramma/dramma-app"
LOCAL_BINARY="target/release/dramma"

echo "🔨 Building dramma in release mode..."

# Clean previous build artifact to ensure fresh RPATH
rm -f target/release/dramma

# Build using nix-shell for consistent environment
# Note: This creates a binary with Nix dependencies (glibc, etc)
nix-shell --run "cargo build --release"

echo "📦 Preparing deployment to ${DRAMMA_HOST} (via root SSH)"

# Stop the service first to release file locks
echo "🛑 Stopping service..."
ssh root@${DRAMMA_HOST} "su - ${DRAMMA_USER} -c 'XDG_RUNTIME_DIR=/run/user/\$(id -u ${DRAMMA_USER}) systemctl --user stop dramma.service' || true"

# Create remote directory structure as root, then chown to dramma user
echo "📁 Creating remote directories..."
ssh root@${DRAMMA_HOST} "mkdir -p ${REMOTE_DIR}/{data,logs,.config} && chown -R ${DRAMMA_USER}:${DRAMMA_USER} ${REMOTE_DIR}"

# Copy binary
echo "🚀 Preparing binary for deployment..."

# Copy required shared libraries for Nix-built binary
echo "📚 Identifying and copying all required libraries..."
ssh root@${DRAMMA_HOST} "mkdir -p ${REMOTE_DIR}/lib"

# Get ALL libraries (Nix and system) using nix-shell to resolve paths correctly
echo "  Analyzing and patching dependencies..."
rm -rf /tmp/dramma-libs
mkdir -p /tmp/dramma-libs

# Get list of libraries from ldd within nix-shell
LIBS=$(nix-shell --run "ldd ${LOCAL_BINARY}" | grep '=> /' | awk '{print $3}')

for lib in $LIBS; do
  libname=$(basename $lib)
  
  # Exclude glibc core libraries to use system versions and avoid conflictsf
  if [[ "$libname" == libc.so* || "$libname" == libm.so* || "$libname" == libpthread.so* || "$libname" == libdl.so* || "$libname" == librt.so* || "$libname" == ld-linux* ]]; then
    
    # Check if it's libstdc++ and force bundle it if ldd found it (though we do explicit check below too)
    # Actually libstdc++ is not glibc, so it won't be caught here.
    echo "  Skipping system library $libname (will use target's version)..."
    continue
  fi

  cp $lib /tmp/dramma-libs/$libname
  echo "  Bundling $libname..."
  nix-shell -p patchelf --run "patchelf --force-rpath --set-rpath '\$ORIGIN' /tmp/dramma-libs/$libname 2>/dev/null || true" 
done

# Explicitly find and bundle libstdc++ as ldd often fails to resolve it in nix-shell
LIBSTDCPP=$(nix-shell --run "gcc --print-file-name=libstdc++.so.6")
if [ -f "$LIBSTDCPP" ]; then
    echo "  Force bundling libstdc++ from $LIBSTDCPP..."
    cp "$LIBSTDCPP" /tmp/dramma-libs/libstdc++.so.6
    nix-shell -p patchelf --run "patchelf --force-rpath --set-rpath '\$ORIGIN' /tmp/dramma-libs/libstdc++.so.6 2>/dev/null || true"
fi

# Copy all patched libraries to target
echo "  Copying patched libraries to target..."
# Remove old libs first to ensure clean state
ssh root@${DRAMMA_HOST} "rm -rf ${REMOTE_DIR}/lib/*"
scp /tmp/dramma-libs/* root@${DRAMMA_HOST}:${REMOTE_DIR}/lib/

# Patch the binary to use the SYSTEM interpreter and include bundled libs in RPATH
echo "🔧 Patching binary for portable deployment (using system interpreter)..."
# We do this patching on the target because locally we might not want to mess up the binary or paths don't match
# But wait, we need to copy it first.

echo "  Deploying binary..."
scp ${LOCAL_BINARY} root@${DRAMMA_HOST}:${REMOTE_DIR}/dramma

ssh root@${DRAMMA_HOST} "chown -R ${DRAMMA_USER}:${DRAMMA_USER} ${REMOTE_DIR}/lib && chmod +x ${REMOTE_DIR}/dramma"

# Ensure patchelf is installed on target for the next step
echo "  Ensuring patchelf is installed on target..."
ssh root@${DRAMMA_HOST} "which patchelf >/dev/null || (apt update && apt install -y patchelf)"

echo "  Patching binary on target..."
ssh root@${DRAMMA_HOST} "patchelf --set-interpreter /lib64/ld-linux-x86-64.so.2 --force-rpath --set-rpath '/home/dramma/dramma-app/lib:/lib/x86_64-linux-gnu:/usr/lib/x86_64-linux-gnu' ${REMOTE_DIR}/dramma"

# Clean up temp directory
rm -rf /tmp/dramma-libs

# Copy configuration
echo "⚙️  Copying configuration..."
scp .config/dramma.toml root@${DRAMMA_HOST}:${REMOTE_DIR}/.config/
ssh root@${DRAMMA_HOST} "chown ${DRAMMA_USER}:${DRAMMA_USER} ${REMOTE_DIR}/.config/dramma.toml"

# Copy any existing database (optional)
if [ -f "data/Stats.db" ]; then
    echo "💾 Copying database..."
    scp data/Stats.db root@${DRAMMA_HOST}:${REMOTE_DIR}/data/
    ssh root@${DRAMMA_HOST} "chown ${DRAMMA_USER}:${DRAMMA_USER} ${REMOTE_DIR}/data/Stats.db"
fi

# Create systemd user service on remote machine
echo "🔧 Setting up systemd service..."
ssh root@${DRAMMA_HOST} "su - ${DRAMMA_USER} -c 'mkdir -p ~/.config/systemd/user'"

ssh root@${DRAMMA_HOST} "su - ${DRAMMA_USER} -c 'cat > ~/.config/systemd/user/dramma.service'" << 'SERVICEEOF'
[Unit]
Description=Dramma Donation Kiosk
After=graphical-session.target

[Service]
Type=simple
Environment=DISPLAY=:0
Environment=RUST_LOG=info
WorkingDirectory=/home/dramma/dramma-app
ExecStart=/home/dramma/dramma-app/dramma
Restart=always
RestartSec=5

[Install]
WantedBy=graphical-session.target
SERVICEEOF

# Reload and enable service (run as dramma user)
echo "🔄 Enabling lingering and systemd service..."
# Enable lingering so systemd user services can run without active session
ssh root@${DRAMMA_HOST} "loginctl enable-linger ${DRAMMA_USER}"

# Reload and enable the service using the proper environment
ssh root@${DRAMMA_HOST} "su - ${DRAMMA_USER} -c 'XDG_RUNTIME_DIR=/run/user/\$(id -u ${DRAMMA_USER}) systemctl --user daemon-reload && XDG_RUNTIME_DIR=/run/user/\$(id -u ${DRAMMA_USER}) systemctl --user enable dramma.service'"

# Create LXQt autostart entry
echo "🖥️  Setting up LXQt autostart..."
ssh root@${DRAMMA_HOST} "su - ${DRAMMA_USER} -c 'mkdir -p ~/.config/autostart'"
ssh root@${DRAMMA_HOST} "su - ${DRAMMA_USER} -c 'cat > ~/.config/autostart/dramma.desktop'" << 'DESKTOPEOF'
[Desktop Entry]
Type=Application
Name=Dramma Kiosk
Exec=systemctl --user start dramma.service
X-LXQt-Need-Tray=false
DESKTOPEOF

echo ""
echo "✅ Deployment complete!"
echo ""
echo "Next steps on the dramma machine (as root):"
echo "1. Ensure dramma user is in 'dialout' group: usermod -a -G dialout dramma"
echo "2. Install chromium: apt install chromium || apt install chromium-browser"
echo "3. Reboot the machine to test autologin and autostart"
echo ""
echo "To restart the service remotely:"
echo "  ssh root@${DRAMMA_HOST} \"su - ${DRAMMA_USER} -c 'XDG_RUNTIME_DIR=/run/user/$(id -u) systemctl --user restart dramma.service'\""
echo ""
echo "To view logs remotely:"
echo "  ssh root@${DRAMMA_HOST} \"su - ${DRAMMA_USER} -c 'XDG_RUNTIME_DIR=/run/user/$(id -u) journalctl --user -u dramma.service -f'\""
