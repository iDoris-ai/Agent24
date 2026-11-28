#!/bin/sh
# Build a24-pdf2docx onedir for macOS arm64.
set -eu

ENGINE_VERSION=0.5.13-a24.1
PYTHON_URL="https://github.com/astral-sh/python-build-standalone/releases/download/20261124/cpython-3.12.16%2B20261124-aarch64-apple-darwin-install_only.tar.gz"
# 与上游 SHA256SUMS 核对过（11-28）。
PYTHON_SHA256=431b99a9c2cdd37ad9b06e425be0e520d2dbce0552abe454257abef592fe3634
# PyMuPDF 的 wheel 只带一行许可说明；AGPL-3.0 全文取其源码仓库同版本的 COPYING（钉 sha256）。
PYMUPDF_COPYING_URL="https://raw.githubusercontent.com/pymupdf/PyMuPDF/1.28.2/COPYING"
PYMUPDF_COPYING_SHA256=57c8ff33c9c0cfc3ef00e650a1cc910d7ee479a8bc509f6c9209a7c2a11399d6

if [ "$#" -ne 2 ]; then
  echo "usage: build.sh <work_dir> <out_dir>" >&2
  exit 2
fi

work_dir=$1
out_dir=$2
here=$(CDPATH= cd "$(dirname "$0")" && pwd)

mkdir -p "$work_dir" "$out_dir"

tarball="$work_dir/cpython-3.12.16+20261124-aarch64-apple-darwin-install_only.tar.gz"
if [ ! -f "$tarball" ]; then
  curl -fsSL -o "$tarball" "$PYTHON_URL"
fi

got=$(shasum -a 256 "$tarball")
got=${got%% *}
if [ "$got" != "$PYTHON_SHA256" ]; then
  echo "sha256 mismatch: got $got expected $PYTHON_SHA256" >&2
  exit 1
fi

if [ ! -x "$work_dir/python/bin/python3" ]; then
  tar -xzf "$tarball" -C "$work_dir"
fi

# 每次新建 venv：旧 venv 里已打过补丁的 pdf2docx 会让 patch --forward 失败（Codex L12-1 C3）。
rm -rf "$work_dir/venv" "$work_dir/wheels"
"$work_dir/python/bin/python3" -m venv "$work_dir/venv"
venv="$work_dir/venv"
# 依赖按 macOS 13 arm64 取 wheel（产品最低系统 13），不用构建机自己的平台：
# 先按锁文件的 hash 下载，再离线装。
wheels="$work_dir/wheels"
"$venv/bin/python" -m pip download --only-binary=:all: --platform macosx_13_0_arm64 \
  --python-version 3.12 --implementation cp --no-deps --require-hashes \
  -r "$here/requirements.lock" -d "$wheels"
"$venv/bin/python" -m pip install --no-index --no-deps --require-hashes \
  --find-links "$wheels" -r "$here/requirements.lock"
"$venv/bin/python" -m pip install pyinstaller==6.22.3

site=$("$venv/bin/python" -c "import sysconfig; print(sysconfig.get_path('purelib'))")
for p in "$here/patches"/*.patch; do
  if [ -f "$p" ]; then
    patch -p1 --forward -d "$site" < "$p"
  fi
done
if grep -q '| fitz.TEXT_MEDIABOX_CLIP' "$site/pdf2docx/page/RawPageFitz.py"; then
  echo "patch failed: TEXT_MEDIABOX_CLIP still present" >&2
  exit 1
fi

mkdir -p "$out_dir/dist"
"$venv/bin/pyinstaller" \
  --onedir \
  --noconfirm \
  --name a24-pdf2docx \
  --workpath "$work_dir/pyinstaller" \
  --specpath "$work_dir" \
  --distpath "$out_dir/dist" \
  "$here/a24_pdf2docx.py"

dist="$out_dir/dist/a24-pdf2docx"

# 产品最低支持 macOS 13（engines/pdfkit/build.sh 同一口径）：包里任何 Mach-O 要求更高就失败。
too_new=$(find "$dist" -type f \( -perm -u+x -o -name '*.dylib' -o -name '*.so' \) -print | while read -r f; do
  minos=$(otool -l "$f" 2>/dev/null | awk '/LC_BUILD_VERSION/{b=1} b&&/minos/{print $2; exit}')
  [ -n "$minos" ] || continue
  major=${minos%%.*}
  if [ "$major" -gt 13 ]; then echo "$minos $f"; fi
done)
if [ -n "$too_new" ]; then
  echo "binaries need a macOS newer than 13:" >&2
  echo "$too_new" >&2
  exit 1
fi
lic="$dist/licenses"
mkdir -p "$lic"

pdf2docx_lic=""
for f in "$site"/pdf2docx-*.dist-info/LICENSE "$site"/pdf2docx-*.dist-info/licenses/LICENSE; do
  if [ -f "$f" ]; then
    pdf2docx_lic=$f
    break
  fi
done
if [ -z "$pdf2docx_lic" ]; then
  echo "pdf2docx LICENSE not found" >&2
  exit 1
fi
cp "$pdf2docx_lic" "$lic/pdf2docx-LICENSE"

# PyMuPDF（含 MuPDF）：AGPL-3.0 全文，钉版下载并核对 sha256（wheel 里只有一行说明，Codex L12-1 C2）。
copying="$work_dir/PyMuPDF-1.28.2-COPYING"
if [ ! -f "$copying" ]; then
  curl -fsSL -o "$copying" "$PYMUPDF_COPYING_URL"
fi
got=$(shasum -a 256 "$copying")
got=${got%% *}
if [ "$got" != "$PYMUPDF_COPYING_SHA256" ]; then
  echo "sha256 mismatch for PyMuPDF COPYING: got $got" >&2
  exit 1
fi
cp "$copying" "$lic/PyMuPDF-COPYING"

pdf2docx_ver=$("$venv/bin/python" -c "from importlib.metadata import version; print(version('pdf2docx'))")
pymupdf_ver=$("$venv/bin/python" -c "from importlib.metadata import version; print(version('pymupdf'))")
{
  echo "PyMuPDF ${pymupdf_ver}"
  echo "https://github.com/pymupdf/PyMuPDF/tree/${pymupdf_ver}"
  echo "MuPDF (bundled in PyMuPDF ${pymupdf_ver}): https://mupdf.com/releases (source of the matching version)"
  echo "pdf2docx ${pdf2docx_ver}"
  echo "https://github.com/ArtifexSoftware/pdf2docx/tree/v0.5.13"
} > "$lic/SOURCE.txt"

cp -R "$here/patches" "$lic/patches"
cp "$here/a24_pdf2docx.py" "$lic/a24_pdf2docx.py"

archive="a24-pdf2docx-${ENGINE_VERSION}-darwin-arm64.tar.gz"
COPYFILE_DISABLE=1 tar -C "$out_dir/dist" -czf "$out_dir/$archive" a24-pdf2docx

sha=$(shasum -a 256 "$out_dir/$archive")
sha=${sha%% *}
echo "$sha  $archive" > "$out_dir/$archive.sha256"
size=$(stat -f %z "$out_dir/$archive")
printf '{"version":"%s","file":"%s","size":%s,"sha256":"%s"}\n' \
  "$ENGINE_VERSION" "$archive" "$size" "$sha"
