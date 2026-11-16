#!/usr/bin/env python3
import hashlib
import io
import os
from pathlib import Path
import platform
import shutil
import subprocess
import sys
import tarfile
import time

def computeSha256(filePath: Path) -> str:
    hasher = hashlib.sha256()
    with open(filePath, "rb") as fileHandle:
        for chunk in iter(lambda: fileHandle.read(65536), b""):
            hasher.update(chunk)
    return hasher.hexdigest()

def detectPlatform() -> str:
    system = platform.system().lower()
    machine = platform.machine().lower()
    if machine in ("x86_64", "amd64"):
        arch = "x86_64"
    elif machine in ("aarch64", "arm64"):
        arch = "aarch64"
    else:
        arch = machine
    if system == "linux":
        return f"linux-gnu-{arch}"
    if system == "windows":
        return f"windows-msvc-{arch}"
    if system == "darwin":
        return f"macos-{arch}"
    return f"{system}-{arch}"

def readPackageVersion(projectDirectory: Path) -> str:
    cargoToml = projectDirectory / "Cargo.toml"
    if not cargoToml.is_file():
        return "0.1.0"
    with open(cargoToml, "r", encoding="utf-8") as fileHandle:
        inPackage = False
        for line in fileHandle:
            stripped = line.strip()
            if stripped == "[package]":
                inPackage = True
                continue
            if inPackage and stripped.startswith("["):
                break
            if inPackage and stripped.startswith("version"):
                parts = stripped.split("=", 1)
                if len(parts) == 2:
                    return parts[1].strip().strip('"').strip("'")
    return "0.1.0"

def buildReleaseBinary(projectDirectory: Path) -> None:
    command = ["cargo", "build", "--release", "--bin", "yukino-lsp"]
    subprocess.run(command, cwd=projectDirectory, check=True)

def generateManifest(version: str, target: str, lspSha: str, lspSize: int) -> str:
    return (
        f'[workspace]\n'
        f'author = "shareui <shareui@proton.me>"\n'
        f'version = "{version}"\n'
        f'version_number = 1\n'
        f'source_code = "https://github.com/yukitsuri/yukino-lsp"\n'
        f'links = ["https://github.com/yukitsuri/yukino-lsp"]\n\n'
        f'[packages."de.shareui.yukinolsp"]\n'
        f'name = "yukino-lsp"\n\n'
        f'[packages."de.shareui.yukinolsp".binaries."{target}"]\n'
        f'file = "yukino-lsp"\n'
        f'size = {lspSize}\n'
        f'sha256 = "{lspSha}"\n'
    )

def createPackageArchive(archivePath: Path, manifestContent: str, lspBin: Path) -> None:
    archivePath.parent.mkdir(parents=True, exist_ok=True)
    with tarfile.open(archivePath, "w:gz") as tarHandle:
        manifestBytes = manifestContent.encode("utf-8")
        manifestInfo = tarfile.TarInfo(name="lacie.toml")
        manifestInfo.size = len(manifestBytes)
        manifestInfo.mode = 0o644
        manifestInfo.mtime = int(time.time())
        tarHandle.addfile(manifestInfo, io.BytesIO(manifestBytes))
        binInfo = tarfile.TarInfo(name="yukino-lsp")
        binInfo.size = lspBin.stat().st_size
        binInfo.mode = 0o755
        binInfo.mtime = int(time.time())
        with open(lspBin, "rb") as binFile:
            tarHandle.addfile(binInfo, binFile)

def installWithLacie(archivePath: Path) -> None:
    lacieExecutable = shutil.which("lacie")
    if not lacieExecutable:
        raise RuntimeError("lacie executable not found in PATH")
    command = [lacieExecutable, "add", str(archivePath.resolve()), "--force"]
    subprocess.run(command, check=True)

def main() -> None:
    scriptDirectory = Path(__file__).resolve().parent
    buildReleaseBinary(scriptDirectory)
    lspBin = scriptDirectory / "target" / "release" / "yukino-lsp"
    if not lspBin.is_file():
        raise RuntimeError("built binary yukino-lsp not found in target/release")
    version = readPackageVersion(scriptDirectory)
    target = detectPlatform()
    lspSha = computeSha256(lspBin)
    lspSize = lspBin.stat().st_size
    manifest = generateManifest(version, target, lspSha, lspSize)
    archivePath = scriptDirectory / "target" / "local-yukino-lsp.tar.gz"
    createPackageArchive(archivePath, manifest, lspBin)
    installWithLacie(archivePath)

if __name__ == "__main__":
    try:
        main()
    except subprocess.CalledProcessError as processError:
        print(f"error: command failed with status {processError.returncode}", file=sys.stderr)
        sys.exit(processError.returncode)
    except KeyboardInterrupt:
        print("error: interrupted", file=sys.stderr)
        sys.exit(130)
