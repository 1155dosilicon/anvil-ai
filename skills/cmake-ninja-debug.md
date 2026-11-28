---
name: cmake-ninja-debug
title: CMake + Ninja Builds and Debugging on Windows
description: Modern CMakeLists.txt templates for Windows x86_64 with Ninja, static C++ runtime for standalone .exe binaries, CMAKE_EXPORT_COMPILE_COMMANDS=ON (for clangd), presets, tests with CTest, and practical debugging with lldb.
---

## Configure, build, test

```powershell
cmake -S . -B build -G Ninja -DCMAKE_BUILD_TYPE=Debug -DCMAKE_EXPORT_COMPILE_COMMANDS=ON
cmake --build build
ctest --test-dir build --output-on-failure
cmake --build build --target clean      # clean
```

Anvil already sets `CMAKE_GENERATOR=Ninja`, `CC=clang.exe`, `CXX=clang++.exe`
and provides `cmake`, `ctest` and `ninja` shims. Re-run the configure step
after editing `CMakeLists.txt` only if the build does not pick it up
automatically. Delete `build\` if the compiler changed.

## Template: standalone Windows C++ executable + library + tests

On Windows (`llvm-mingw`), link C++ executables with `-static` so they run on
any Windows PC (and under `ctest`) without needing `libc++.dll` or
`libunwind.dll` on `PATH`. Keep it behind an option so sanitizer builds or DLL
targets can disable it cleanly:

```cmake
cmake_minimum_required(VERSION 3.20)
project(myapp VERSION 0.1.0 LANGUAGES CXX)   # add C for C sources

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)
set(CMAKE_EXPORT_COMPILE_COMMANDS ON)       # compile_commands.json for clangd

option(ENABLE_SANITIZERS "Build with ASan + UBSan" OFF)
option(STATIC_RUNTIME "Link C++ runtime statically for standalone Windows .exe" ON)

add_library(core src/core.cpp)
target_include_directories(core PUBLIC include)
target_compile_options(core PRIVATE -Wall -Wextra -Wpedantic)

add_executable(myapp src/main.cpp)
target_link_libraries(myapp PRIVATE core)

if(ENABLE_SANITIZERS)
  add_compile_options(-fsanitize=address,undefined -fno-omit-frame-pointer)
  add_link_options(-fsanitize=address,undefined)
elseif(STATIC_RUNTIME)
  # Standalone .exe: no runtime dependency on libc++.dll / libunwind.dll
  target_link_options(myapp PRIVATE -static)
endif()

include(CTest)
if(BUILD_TESTING)
  add_executable(core_tests tests/core_tests.cpp)
  target_link_libraries(core_tests PRIVATE core)
  if(STATIC_RUNTIME AND NOT ENABLE_SANITIZERS)
    target_link_options(core_tests PRIVATE -static)
  endif()
  add_test(NAME core_tests COMMAND core_tests)
endif()
```

For C: `project(x LANGUAGES C)`, `set(CMAKE_C_STANDARD 17)`,
`set(CMAKE_C_STANDARD_REQUIRED ON)`.

Rules: use `target_*` commands (never global `include_directories`), keep
sources listed explicitly (no `file(GLOB)`), one target per library/program.
The MCP tool `cc_build` seeds fresh CMake build directories with
`-DCMAKE_EXE_LINKER_FLAGS_INIT=-static` by default (skipped with sanitizers —
`-fsanitize=`, `-D...SANITIZ...=ON` — or `-DBUILD_SHARED_LIBS=ON`; an already
configured build dir keeps its cached flags). Pass `static_link: true` to force
`-DCMAKE_EXE_LINKER_FLAGS=-static`, or `static_link: false` to opt out.

## clangd sees nothing?

1. Configure with `-DCMAKE_EXPORT_COMPILE_COMMANDS=ON`.
2. Copy `build\compile_commands.json` to the project root, or create `.clangd`:

```yaml
CompileFlags:
  CompilationDatabase: build
```

3. Restart clangd / the editor. Headers are resolved from the compile command.

## CMakePresets.json (optional, reproducible)

```json
{
  "version": 6,
  "configurePresets": [
    { "name": "debug", "generator": "Ninja", "binaryDir": "build",
      "cacheVariables": { "CMAKE_BUILD_TYPE": "Debug", "CMAKE_EXPORT_COMPILE_COMMANDS": "ON" } }
  ],
  "buildPresets": [ { "name": "debug", "configurePreset": "debug" } ]
}
```

`cmake --preset debug && cmake --build --preset debug`.

## Debugging with lldb

```powershell
lldb build\myapp.exe -- arg1 arg2
(lldb) b main                 # or: b src/core.cpp:42
(lldb) r                      # run
(lldb) n / s / finish         # next, step into, step out
(lldb) bt                     # backtrace after a crash
(lldb) fr v                   # local variables
(lldb) p expr                 # evaluate
(lldb) watchpoint set variable x
```

Non-interactive (good for agents):

```powershell
lldb --batch -o run -o bt -- build\myapp.exe arg1
```

## Common failures

- Exit code `-1073741515` (`0xC0000135`) when running a C++ `.exe` -> missing
  `libc++.dll` / `libunwind.dll`. Link the executable with `-static` (or set
  `STATIC_RUNTIME=ON`).
- `CMAKE_CXX_COMPILER not set` -> run through Anvil's shims (open a new terminal
  or run `anvil env | Invoke-Expression`), then delete `build\` and configure again.
- `ninja: error: loading 'build.ninja'` -> configure step not run or failed.
- Link errors `undefined reference` -> missing `target_link_libraries`.
- Crash without symbols -> build with `-DCMAKE_BUILD_TYPE=Debug` (or `RelWithDebInfo`).
