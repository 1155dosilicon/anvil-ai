# Changelog

## 0.2.1

Documentação e textos de ajuda alinhados com a 0.2.0.

- `doctor --smoke-test` (CLI e MCP `anvil_doctor`) descreve os 4 casos: C, C++ (`-static`), Rust e Rust `gnullvm`; coluna da saída alinhada.
- `AGENTS.md`: smoke tests `"skipped"` (toolchain não instalado) não contam como falha; reinstalar é o caminho de atualização.
- `START_HERE.md`, skill `anvil-toolkit` e README: como atualizar pendrive e PC, `cc_build` standalone por padrão (inclusive CMake) e `+crt-static` no `gnullvm`.

## 0.2.0

**Só Windows x86_64.** Mudança incompatível com a 0.1.

- Removidos: plataforma `linux-x86_64`, `install.sh`, `bundle init-usb --linux-bin`, shells bash/zsh/fish no `anvil env`, integração com perfil Linux e receitas Linux.
- Shims removidos: `lld`, `pkg-config`, `pkgconf`, `gdb` (apagados no `install`/`uninstall`). Shims novos: `llvm-ar`, `llvm-ranlib`, `llvm-windres`, `windres`, `llvm-objdump`, `llvm-nm`, `llvm-strip`, `rustdoc`, `ctest`.
- Pendrives da 0.1 continuam funcionando: bundles de plataformas não suportadas no `manifest.json` são ignorados com aviso; `bundle init-usb` remove os restos de Linux e `bundle verify` avisa sobre eles.
- `cc_build`: C++ standalone por padrão (`-static` no arquivo único; `CMAKE_EXE_LINKER_FLAGS_INIT=-static` em builds CMake novos), desligado com sanitizers ou `BUILD_SHARED_LIBS=ON`.
- Rust `gnullvm`: `CARGO_TARGET_X86_64_PC_WINDOWS_GNULLVM_RUSTFLAGS=-C target-feature=+crt-static` por padrão (sem `libunwind.dll`).
- `RUSTC` aponta para o shim do `rustc`, para build scripts do host não verem o `llvm\bin`.
- `doctor --smoke-test` roda cada binário com `PATH` limpo e ganhou o caso Rust `gnullvm`.
- Extração: um loop de symlink (`a/b -> ..`) é recusado no fallback por cópia (antes recursava até estourar o tamanho do caminho).
- Erro de compilação claro fora do Windows.

## 0.1.0

Primeira versão: toolchain offline (llvm-mingw, Rust, CMake/Ninja) para Windows e Linux, shims multicall, servidor MCP com ponte LSP, configuração automática de agentes e construtor de pendrive.
