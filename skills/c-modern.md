---
name: c-modern
title: Modern C (C11/C17/C23) with Clang on Windows
description: Write, build and debug modern C11/C17/C23 on Windows x86_64 with Clang (llvm-mingw UCRT) - strict warnings, sanitizers, compile_commands.json, clang-tidy, clangd and Win32 APIs.
---

## Compile (single file)

```powershell
clang -std=c17 -Wall -Wextra -Wpedantic -Werror -g -O0 main.c -o main.exe
clang -std=c17 -O2 -DNDEBUG main.c -o main.exe            # release
clang -std=c23 -Wall -Wextra -Wpedantic main.c -o main.exe # C23 (Clang 20)
```

Plain C binaries compiled with Anvil (`llvm-mingw` UCRT) link against the
standard Windows UCRT and run standalone on Windows 10/11 without extra DLLs.
If you link `pthreads` (`-pthread`), add `-static` to avoid needing
`libwinpthread-1.dll` at runtime. Use CMake (`cmake-ninja-debug` skill) as soon
as there is more than one file.

## Sanitizers (use them for tests)

```powershell
clang -std=c17 -g -O1 -fno-omit-frame-pointer `
      -fsanitize=address,undefined main.c -o main.exe
```

- ASan reports heap/stack overflow, use-after-free and double free.
- UBSan reports signed overflow, misaligned access, bad shifts and null deref.
- Do **not** combine `-static` with `-fsanitize=address`: ASan on `llvm-mingw`
  uses `libclang_rt.asan_dynamic-x86_64.dll` (run under `lldb` or with
  `$env:ANVIL_HOME\toolchains\llvm\bin` on `PATH` while testing).
- If ASan linking is not suitable for a specific target, keep
  `-fsanitize=undefined -fsanitize-trap=undefined`.
- Read the FIRST report: it shows the faulting line and the allocation site.

## Safe C rules

- Every `malloc` checked for `NULL`; every allocation has one clear owner and one `free`.
- Sizes are `size_t`; check `a > SIZE_MAX / b` before `a * b`.
- No `gets`, `strcpy`, `strcat`, `sprintf`: use `snprintf`, `memcpy` with explicit lengths.
- Initialize all variables; `struct foo x = {0};`.
- Return error codes (`0` ok, negative error codes) and check every one.
- `static` for file-local functions; headers have include guards or `#pragma once`.
- Fixed-width types from `<stdint.h>`, `bool` from `<stdbool.h>` (keyword in C23).

## Tooling

```powershell
clang-format -i src\*.c include\*.h               # format (style in .clang-format)
clang-tidy src\main.c -- -std=c17 -Iinclude       # lint one file without a build system
clang-tidy -p build src\main.c                    # lint using build\compile_commands.json
```

Minimal `.clang-tidy`:

```yaml
Checks: 'clang-analyzer-*,bugprone-*,cert-*,misc-*,-misc-unused-parameters'
WarningsAsErrors: 'clang-analyzer-*,bugprone-*'
```

**clangd** needs `compile_commands.json` at the project root (or in `build\`):
generate it with CMake (`-DCMAKE_EXPORT_COMPILE_COMMANDS=ON`) and copy it to
the root (or add `CompileFlags: { CompilationDatabase: build }` to `.clangd`).
Without CMake, write `compile_flags.txt` with one flag per line:

```text
-std=c17
-Iinclude
-Wall
```

## Windows specifics (`llvm-mingw` UCRT)

- Prefer standard C (`<stdio.h>`, `<stdlib.h>`, `<string.h>`, `<time.h>`) over raw Win32 when possible.
- For Win32 APIs, include `<windows.h>` (define `WIN32_LEAN_AND_MEAN` and `NOMINMAX` before it) and link required import libraries (e.g. `-luser32`, `-lws2_32`).
- For `.rc` resources/icons, compile with `windres` or `llvm-windres` (both shimmed by Anvil).
- Open binary files with `"rb"` / `"wb"` to avoid `\r\n` text translation.
- `long` is 32-bit on Windows x64 (LLP64): always use `int64_t` / `size_t` for 64-bit integers and sizes.

## Debug

`lldb .\main.exe` -> `b main`, `r`, `n`, `s`, `bt`, `p var`, `fr v` (frame variables).
