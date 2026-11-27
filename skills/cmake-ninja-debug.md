---
name: cmake-ninja-debug
title: CMake + Ninja Builds and Debugging
description: Modern CMakeLists.txt templates with Ninja and CMAKE_EXPORT_COMPILE_COMMANDS=ON (so clangd works immediately), presets, tests with CTest, and practical debugging with lldb and gdb.
---

## Configure, build, test

```sh
cmake -S . -B build -G Ninja -DCMAKE_BUILD_TYPE=Debug -DCMAKE_EXPORT_COMPILE_COMMANDS=ON
cmake --build build
ctest --test-dir build --output-on-failure
cmake --build build --target clean      # clean
```

Anvil already sets `CMAKE_GENERATOR=Ninja`, `CC=clang`, `CXX=clang++`.
Re-run the configure step after editing `CMakeLists.txt` only if the build
does not pick it up automatically. Delete `build/` if the compiler changed.

## Template: C++ executable + library + tests

```cmake
cmake_minimum_required(VERSION 3.20)
project(myapp VERSION 0.1.0 LANGUAGES CXX)   # add C for C sources

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)
set(CMAKE_EXPORT_COMPILE_COMMANDS ON)       # compile_commands.json for clangd

add_library(core src/core.cpp)
target_include_directories(core PUBLIC include)
target_compile_options(core PRIVATE -Wall -Wextra -Wpedantic)

add_executable(myapp src/main.cpp)
target_link_libraries(myapp PRIVATE core)

option(ENABLE_SANITIZERS "Build with ASan + UBSan" OFF)
if(ENABLE_SANITIZERS)
  add_compile_options(-fsanitize=address,undefined -fno-omit-frame-pointer)
  add_link_options(-fsanitize=address,undefined)
endif()

include(CTest)
if(BUILD_TESTING)
  add_executable(core_tests tests/core_tests.cpp)
  target_link_libraries(core_tests PRIVATE core)
  add_test(NAME core_tests COMMAND core_tests)
endif()
```

For C: `project(x LANGUAGES C)`, `set(CMAKE_C_STANDARD 17)`,
`set(CMAKE_C_STANDARD_REQUIRED ON)`.

Rules: use `target_*` commands (never global `include_directories`), keep
sources listed explicitly (no `file(GLOB)`), one target per library/program.

## clangd sees nothing?

1. Configure with `-DCMAKE_EXPORT_COMPILE_COMMANDS=ON`.
2. Copy `build/compile_commands.json` to the project root, or create `.clangd`:

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

## Debugging with lldb (Windows + Linux)

```sh
lldb build/myapp -- arg1 arg2
(lldb) b main                 # or: b src/core.cpp:42
(lldb) r                      # run
(lldb) n / s / finish         # next, step into, step out
(lldb) bt                     # backtrace after a crash
(lldb) fr v                   # local variables
(lldb) p expr                 # evaluate
(lldb) watchpoint set variable x
```

Non-interactive (good for agents):

```sh
lldb --batch -o run -o bt -- build/myapp arg1
```

## gdb (Linux)

```sh
gdb --args build/myapp arg1
gdb -batch -ex run -ex bt --args build/myapp arg1    # non-interactive crash report
```

Same verbs: `break`, `run`, `next`, `step`, `bt`, `info locals`, `print`.

## Common failures

- `CMAKE_CXX_COMPILER not set` -> run through Anvil's shims (open a new terminal
  or `eval "$(anvil env)"`), then delete `build/` and configure again.
- `ninja: error: loading 'build.ninja'` -> configure step not run or failed.
- Link errors `undefined reference` -> missing `target_link_libraries`.
- Crash without symbols -> build with `-DCMAKE_BUILD_TYPE=Debug` (or `RelWithDebInfo`).
