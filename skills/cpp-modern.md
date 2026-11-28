---
name: cpp-modern
title: Modern C++ (C++20/23) with Clang on Windows
description: Write and diagnose modern C++20/23 on Windows x86_64 with Clang and libc++ - standalone static linking, RAII, smart pointers, std::span, ranges, clang-format, clang-tidy and clangd diagnostics.
---

## Compile (standalone Windows `.exe`)

Anvil ships `llvm-mingw` (`clang++` + `libc++` + `ld.lld`). Because
`ANVIL_HOME\toolchains\llvm\bin` is **not** on the user `PATH`, a C++ binary
linked dynamically depends on `libc++.dll` and `libunwind.dll` and will fail
outside Anvil with `0xC0000135` (`STATUS_DLL_NOT_FOUND`).

**Always pass `-static` when building standalone C++ executables on Windows:**

```powershell
clang++ -std=c++20 -static -Wall -Wextra -Wpedantic -Werror -g main.cpp -o main.exe
clang++ -std=c++23 -static -O2 -DNDEBUG main.cpp -o main.exe
```

- The MCP tool `cc_build` passes `-static` **automatically** for single-file
  C++ builds unless `-fsanitize=`, `-shared`, `-c` or `static_link: false` is
  specified, and seeds fresh CMake build directories with
  `CMAKE_EXE_LINKER_FLAGS_INIT=-static` (see the `cmake-ninja-debug` skill).
- **Exception — sanitizers & DLLs:** do **not** pass `-static` with
  `-fsanitize=address` (ASan requires `libclang_rt.asan_dynamic-x86_64.dll`) or
  when building shared libraries (`-shared`):

```powershell
clang++ -std=c++20 -g -O1 -fno-omit-frame-pointer -fsanitize=address,undefined main.cpp -o main.exe
# Run with llvm\bin on PATH (or via lldb) during sanitizer testing:
$env:PATH = "$env:ANVIL_HOME\toolchains\llvm\bin;$env:PATH"; .\main.exe
```

For more than one source file, use CMake (`cmake-ninja-debug` skill).

## Core rules

- **RAII**: every resource (memory, file, handle, lock, socket) is owned by an
  object whose destructor releases it. No naked `new`/`delete`.
- Ownership: `std::unique_ptr<T>` (default, `std::make_unique`),
  `std::shared_ptr<T>` only for real shared ownership; raw `T*`/`T&` = non-owning.
- Views: take `std::span<const T>` for contiguous ranges and `std::string_view`
  for read-only strings; never store a view longer than its source lives.
- Containers: `std::vector` by default; `std::array` for fixed size.
- Rule of zero: let members manage resources; if you write one of
  destructor/copy/move, write (or `= delete`) all five.
- `const` by default; `constexpr` for compile-time values; `[[nodiscard]]` on
  functions whose result must be checked.
- Errors: exceptions for exceptional failures, `std::optional<T>` for "no value",
  `std::expected<T, E>` (C++23) for recoverable errors.
- Prefer `enum class`, `auto` where the type is obvious, range-`for`,
  `std::ranges::sort(v)` and `<algorithm>` over hand-written loops.

```cpp
#include <memory>
#include <span>
#include <vector>

[[nodiscard]] int sum(std::span<const int> xs) {
    int total = 0;
    for (int x : xs) total += x;
    return total;
}

struct Buffer {
    std::unique_ptr<std::byte[]> data;
    std::size_t size;
    explicit Buffer(std::size_t n) : data(std::make_unique<std::byte[]>(n)), size(n) {}
};
```

## Format, lint, diagnose

```powershell
clang-format -i src\*.cpp include\*.hpp
clang-tidy -p build src\main.cpp                 # uses build\compile_commands.json
clang-tidy src\main.cpp -- -std=c++20 -Iinclude  # without a build system
```

Minimal `.clang-tidy`:

```yaml
Checks: 'bugprone-*,cppcoreguidelines-*,modernize-*,performance-*,readability-*,-modernize-use-trailing-return-type,-readability-magic-numbers'
WarningsAsErrors: 'bugprone-*'
```

**clangd**: generate `compile_commands.json` with
`cmake -B build -G Ninja -DCMAKE_EXPORT_COMPILE_COMMANDS=ON` and copy it to the
project root (or add `CompileFlags: { CompilationDatabase: build }` to `.clangd`).
Then diagnostics, go-to-definition and hover work in any editor / the
`anvil mcp` LSP bridge. If clangd reports "file not found" for headers, the
include path is missing from the compile command - fix CMake, not the code.

## Reading compiler errors

- Fix the FIRST error; template errors cascade.
- For template failures read the "required from here" / "note: candidate" lines
  and the innermost `in instantiation of` location.
- `-ftemplate-backtrace-limit=0` shows the full chain; `-fdiagnostics-color` helps humans.

## Debug

`lldb .\main.exe` -> `b file.cpp:42`, `r`, `bt`, `fr v`, `p obj.size()`.
libc++ containers are pretty-printed by lldb.
