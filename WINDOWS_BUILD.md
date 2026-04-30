# Building VoiceTypr from Source on Windows

This fork carries two Windows-specific patches that require building from source:

1. **tao#1140 crash fix** — `assert!(flush_paint_messages)` panic during WebView2 teardown, triggered by OBS injecting its Vulkan hook into the process. Patched to a guarded `if`.
2. **bindgen 0.71 / MSVC opaque-struct fix** — `whisper_full_params` is generated as a 1-byte placeholder on MSVC but has a 296-byte size assertion, causing a compile-time underflow. Fixed via pre-generated Windows bindings in a local whisper-rs-sys fork.

---

## Prerequisites

Install all of these before cloning:

| Tool | Where |
|------|-------|
| Visual Studio 2022 Community (with C++ workload) | https://visualstudio.microsoft.com/ |
| Rust (stable, x86_64-pc-windows-msvc) | https://rustup.rs/ |
| LLVM / clang | https://releases.llvm.org/ — install to `C:\Program Files\LLVM` |
| Vulkan SDK 1.4.x | https://vulkan.lunarg.com/ — install to `C:\VulkanSDK\<version>` |
| Ninja build tool | Via VS installer (CMake + Ninja component) or https://ninja-build.org/ |
| Node.js 20+ | https://nodejs.org/ |
| pnpm | `npm install -g pnpm` |

---

## Directory structure

The three repos must be cloned as siblings under a common parent (e.g. `C:\AI Projects\VoiceTyprBuild\`). The `Cargo.toml` uses relative paths `../../tao` and `../../whisper-rs-sys`.

```
VoiceTyprBuild\
  tao\               ← mikemctyre/tao  (branch: windows-tao-1140-fix)
  whisper-rs-sys\    ← mikemctyre/whisper-rs-sys  (branch: master)
  voicetypr\         ← mikemctyre/voicetypr  (branch: main)
```

```batch
mkdir "C:\AI Projects\VoiceTyprBuild"
cd /d "C:\AI Projects\VoiceTyprBuild"
git clone https://github.com/mikemctyre/tao.git
cd tao && git checkout windows-tao-1140-fix && cd ..
git clone https://github.com/mikemctyre/whisper-rs-sys.git
git clone https://github.com/mikemctyre/voicetypr.git
```

---

## Build

Create a batch file (e.g. `build.bat`) with the following content and run it from a normal command prompt — **not** from PowerShell (PowerShell's `-RedirectStandardOutput` deadlocks when the child produces large output):

```batch
@echo off
call "C:\Program Files\Microsoft Visual Studio\2022\Community\VC\Auxiliary\Build\vcvars64.bat"
if %errorlevel% neq 0 exit /b %errorlevel%

set PATH=C:\Users\%USERNAME%\.cargo\bin;C:\Program Files (x86)\Microsoft Visual Studio\Installer;C:\Program Files\Microsoft Visual Studio\2022\Community\Common7\IDE\CommonExtensions\Microsoft\CMake\Ninja;%PATH%
set CMAKE_GENERATOR=Ninja
set CARGO_TARGET_DIR=C:\vt
set VULKAN_SDK=C:\VulkanSDK\1.4.341.1
set LIBCLANG_PATH=C:\Program Files\LLVM\bin

cd /d "C:\AI Projects\VoiceTyprBuild\voicetypr"
pnpm install
pnpm tauri build > build-out.log 2> build-err.log
```

> **Why `CARGO_TARGET_DIR=C:\vt`?** MSVC has a ~250-character object file path limit. The default target dir inside the repo produces paths that exceed this during whisper.cpp compilation.
>
> **Why `CMAKE_GENERATOR=Ninja`?** MSBuild's `ExternalProject_Add` parallelism is broken for whisper.cpp on Windows; Ninja builds it correctly.

---

## Output

On success, the installer is at:

```
C:\vt\release\bundle\nsis\Voicetypr_1.12.2_x64-setup.exe
```

Run it to install. The binary lands at `%LOCALAPPDATA%\VoiceTypr\voicetypr.exe`.

---

## Notes

- The first transcription after a cold launch takes a few extra seconds — this is normal. Vulkan compiles its GPU programs on first use; every recording after that runs at full speed (~300–400ms).
- If OBS Studio is installed, its Vulkan implicit layer is injected into VoiceTypr. This was the original cause of the tao#1140 crash. The tao patch makes VoiceTypr resilient to this; no OBS changes needed.
- The `large-v3-turbo` model is not included in the build — download it from inside the VoiceTypr UI after first launch.
