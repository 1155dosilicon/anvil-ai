---
name: c-modern
title: Modern C (C11/C17/C23) with Clang
description: Write, build and debug portable C11/C17/C23 with Clang - strict warnings, AddressSanitizer/UBSan, compile_commands.json, clang-tidy, clangd and Win32/POSIX portability.
---

## Compile (single file)

```sh
clang -std=c17 -Wall -Wextra -Wpedantic -Werror -g -O0 main.c -o main
clang -std=c17 -O2 -DNDEBUG main.c -o main            # release
clang -std=c2x ...                                     # C23 features (clang >= 18: -std=c23)
```

Add `.exe` to `-o` on Windows (`-o main.exe`). Use CMake (see the
`cmake-ninja-debug` skill) as soon as there is more than one file.

## Sanitizers (use them for every test run)

```sh
clang -std=c17 -g -O1 -fno-omit-frame-pointer \
      -fsanitize=address,undefined main.c -o main && ./main
```

- ASan reports heap/stack overflow, use-after-free, double free, leaks (Linux).
- UBSan reports signed overflow, misaligned access, bad shifts, null deref.
- On Windows (llvm-mingw) `-fsanitize=address` is supported for x86_64;
  if linking fails, keep `-fsanitize=undefined -fsanitize-trap=undefined`.
- Read the FIRST report: it shows the faulting line and the allocation site.

## Safe C rules

- Every `malloc` checked for `NULL`; every allocation has one clear owner and one `free`.
- Sizes are `size_t`; check `a > SIZE_MAX / b` before `a * b`.
- No `gets`, `strcpy`, `strcat`, `sprintf`: use `snprintf`, `memcpy` with explicit lengths.
- Initialize all variables; `struct foo x = {0};`.
- Return error codes (`0` ok, negative errno-style) and check every one.
- `static` for file-local functions; headers have include guards or `#pragma once`.
- Fixed-width types from `<stdint.h>`, `bool` from `<stdbool.h>` (keyword in C23).

## Tooling

```sh
clang-format -i src/*.c include/*.h               # format (style in .clang-format)
clang-tidy src/main.c -- -std=c17 -Iinclude       # lint one file without a build system
clang-tidy -p build src/main.c                    # lint using build/compile_commands.json
```

Minimal `.clang-tidy`:

```yaml
Checks: 'clang-analyzer-*,bugprone-*,cert-*,misc-*,-misc-unused-parameters'
WarningsAsErrors: 'clang-analyzer-*,bugprone-*'
```

**clangd** needs `compile_commands.json` at the project root (or in `build/`):
generate it with CMake (`-DCMAKE_EXPORT_COMPILE_COMMANDS=ON`) and symlink or
copy it to the root. Without CMake, write `compile_flags.txt` with one flag per line:

```text
-std=c17
-Iinclude
-Wall
```

## Win32 / POSIX portability

```c
#if defined(_WIN32)
#  include <windows.h>
#  define PATH_SEP '\\'
#else
#  include <unistd.h>
#  define PATH_SEP '/'
#endif
```

- Prefer standard C (`<stdio.h>`, `<stdlib.h>`, `<string.h>`, `<time.h>`) over OS APIs.
- Threads: C11 `<threads.h>` is not available everywhere; use `pthread` (POSIX
  and llvm-mingw provide winpthreads) or `CreateThread` behind a small wrapper.
- Open files in binary mode (`"rb"`, `"wb"`) to avoid `\r\n` translation on Windows.
- `long` is 32-bit on Windows, 64-bit on Linux: use `int64_t` / `size_t`.

## Debug

`lldb ./main` -> `b main`, `r`, `n`, `s`, `bt`, `p var`, `fr v` (frame variables).
