#!/bin/sh
# Installs the vorp relay and agent binary on Linux and macOS (amd64, arm64).
#
#   curl -fsSL https://get-vorp.s3.ap-south-1.amazonaws.com/install.sh | sudo sh
set -eu

S3_BASE="${VORP_DOWNLOAD_BASE:-https://get-vorp.s3.ap-south-1.amazonaws.com}"
INSTALL_DIR="/usr/local/bin"
SIGNING_KEY_FINGERPRINT="8D623B104588BCF08D40CD85A90F7A794E9AC93F" # gitleaks:allow -- public OpenPGP fingerprint

detect_os() {
    case "$(uname -s)" in
        Linux)  echo "linux" ;;
        Darwin) echo "darwin" ;;
        *) echo "unsupported operating system: $(uname -s)" >&2; exit 1 ;;
    esac
}

detect_arch() {
    case "$(uname -m)" in
        x86_64)        echo "amd64" ;;
        aarch64|arm64) echo "arm64" ;;
        *) echo "unsupported architecture: $(uname -m)" >&2; exit 1 ;;
    esac
}

require_command() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "required command not found: $1" >&2
        exit 1
    fi
}

download_release_metadata() {
    metadata_dir=$1
    require_command curl
    require_command gpg

    curl -fsSL "${S3_BASE}/vorp.asc" -o "${metadata_dir}/vorp.asc"
    # Print fingerprints for primary keys only. Multiple primary keys produce
    # multiple lines and therefore fail the exact comparison below.
    fingerprint=$(gpg --batch --quiet --show-keys --with-colons "${metadata_dir}/vorp.asc" |
        awk -F: '$1 == "pub" { want_fpr = 1; next }
            want_fpr && $1 == "fpr" { print $10; want_fpr = 0 }')
    if [ "$fingerprint" != "$SIGNING_KEY_FINGERPRINT" ]; then
        echo "release signing key fingerprint mismatch" >&2
        exit 1
    fi

    GNUPGHOME="${metadata_dir}/gnupg"
    export GNUPGHOME
    mkdir -m 700 "$GNUPGHOME"
    gpg --batch --quiet --import "${metadata_dir}/vorp.asc"
    curl -fsSL "${S3_BASE}/release-manifest.json" -o "${metadata_dir}/release-manifest.json"
    curl -fsSL "${S3_BASE}/release-manifest.json.asc" -o "${metadata_dir}/release-manifest.json.asc"
    gpg --batch --verify \
        "${metadata_dir}/release-manifest.json.asc" \
        "${metadata_dir}/release-manifest.json"
}

# Verifies "<sha256>  <file>" lines on standard input. macOS has shasum but
# not always sha256sum.
sha256_check() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum -c -
    else
        shasum -a 256 -c -
    fi
}

install_binary_dependencies() {
    missing=""
    for command in curl gpg jq tar; do
        if ! command -v "$command" >/dev/null 2>&1; then
            missing="${missing} ${command}"
        fi
    done
    if ! command -v sha256sum >/dev/null 2>&1 && ! command -v shasum >/dev/null 2>&1; then
        missing="${missing} sha256sum"
    fi
    if [ -z "$missing" ]; then
        return
    fi

    if [ "$os" = "darwin" ]; then
        # Homebrew refuses to run as root, so ask instead of installing.
        echo "missing required commands:${missing}" >&2
        echo "install them with: brew install gnupg jq   then rerun this installer" >&2
        exit 1
    fi

    echo "Installing vorp verification prerequisites:${missing}"
    if command -v apt-get >/dev/null 2>&1; then
        apt-get update -q
        apt-get install -y ca-certificates curl gnupg jq coreutils tar
    elif command -v apk >/dev/null 2>&1; then
        apk add --no-cache ca-certificates curl gnupg jq coreutils tar
    elif command -v dnf >/dev/null 2>&1; then
        dnf install -y ca-certificates curl gnupg2 jq coreutils tar
    elif command -v yum >/dev/null 2>&1; then
        yum install -y ca-certificates curl gnupg2 jq coreutils tar
    elif command -v zypper >/dev/null 2>&1; then
        zypper --non-interactive install ca-certificates curl gpg2 jq coreutils tar
    else
        echo "missing required commands:${missing}" >&2
        echo "install curl, gpg, jq, sha256sum, and tar, then rerun this installer" >&2
        exit 1
    fi
}

install_binary() {
    os=$(detect_os)
    arch=$(detect_arch)
    if [ "$os" = "darwin" ]; then
        # sudo may drop Homebrew's directories from PATH.
        PATH="$PATH:/opt/homebrew/bin:/usr/local/bin"
    fi
    install_binary_dependencies
    require_command jq
    require_command tar

    tmpdir=$(mktemp -d)
    trap 'rm -rf "$tmpdir"' EXIT HUP INT TERM
    download_release_metadata "$tmpdir"

    artifact=$(jq -er --arg os "$os" --arg arch "$arch" '
        [.artifacts[] | select(.os == $os and .arch == $arch)] |
        if length == 1 then .[0] else error("expected exactly one matching artifact") end
    ' "${tmpdir}/release-manifest.json")
    version=$(jq -er '.version | select(test("^[0-9]+\\.[0-9]+\\.[0-9]+([+-][0-9A-Za-z.-]+)?$"))' \
        "${tmpdir}/release-manifest.json")
    filename=$(printf '%s' "$artifact" | jq -er '.filename | select(test("^[A-Za-z0-9._-]+$"))')
    checksum=$(printf '%s' "$artifact" | jq -er '.sha256 | select(test("^[0-9a-fA-F]{64}$"))')
    size=$(printf '%s' "$artifact" | jq -er '.size | select(type == "number" and . > 0 and . <= 52428800 and floor == .)')

    echo "Downloading vorp ${version} (${os} ${arch})..."
    curl -fsSL "${S3_BASE}/${filename}" -o "${tmpdir}/vorp.tar.gz"
    actual_size=$(wc -c < "${tmpdir}/vorp.tar.gz" | tr -d ' ')
    if [ "$actual_size" != "$size" ]; then
        echo "archive size mismatch: got ${actual_size}, want ${size}" >&2
        exit 1
    fi
    printf '%s  %s\n' "$checksum" "${tmpdir}/vorp.tar.gz" | sha256_check

    mkdir "${tmpdir}/extract"
    tar -xzf "${tmpdir}/vorp.tar.gz" -C "${tmpdir}/extract" vorp
    if [ ! -f "${tmpdir}/extract/vorp" ] || [ -L "${tmpdir}/extract/vorp" ]; then
        echo "archive does not contain a regular vorp binary" >&2
        exit 1
    fi
    chmod 755 "${tmpdir}/extract/vorp"
    reported_version=$("${tmpdir}/extract/vorp" --version)
    if [ "$reported_version" != "vorp ${version}" ]; then
        echo "downloaded binary reports version ${reported_version}, want ${version}" >&2
        exit 1
    fi

    mkdir -p "$INSTALL_DIR"
    staged="${INSTALL_DIR}/.vorp.new.$$"
    install -m 755 "${tmpdir}/extract/vorp" "$staged"
    if [ -f "${INSTALL_DIR}/vorp" ]; then
        rollback="${INSTALL_DIR}/.vorp.rollback.$$"
        cp -p "${INSTALL_DIR}/vorp" "$rollback"
        mv -f "$rollback" "${INSTALL_DIR}/vorp.rollback"
    fi
    mv -f "$staged" "${INSTALL_DIR}/vorp"
    echo "vorp installed to ${INSTALL_DIR}/vorp"
    if [ -f "${INSTALL_DIR}/vorp.rollback" ]; then
        echo "previous binary preserved at ${INSTALL_DIR}/vorp.rollback"
    fi
}

install_binary
