# ⚒️ Anvil

[![CI](https://github.com/1155dosilicon/anvil-ai/actions/workflows/ci.yml/badge.svg)](https://github.com/1155dosilicon/anvil-ai/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-green.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/platform-Windows%20x64%20%7C%20Linux%20x64-0078D6.svg)](#escopo)
[![Rust](https://img.shields.io/badge/Rust-%3E%3D1.85-orange.svg)](Cargo.toml)

**Canivete suíço portátil e 100% offline em USB para agentes de IA** (Windows x86_64 e Linux x86_64).

Sempre que você reinstala o sistema operacional, perde horas baixando e configurando compiladores C/C++, Rust, LLVM/Clang, CMake, Ninja, servidores LSP (`clangd`, `rust-analyzer`), servidores MCP e Skills de programação. O **Anvil** resolve isso em **um único binário nativo** (~4 MB, zero dependências) pensado para que **qualquer agente de IA — inclusive modelos abertos menores — plugue o pendrive, instale, valide e já saia programando**.

---

## 🎯 Escopo

### O que o Anvil faz
- **Pendrive mínimo + instalação rápida no SSD interno (sem admin/sudo):** em vez de carregar 50.000 arquivos soltos num pendrive lento (5 MB/s), os toolchains ficam compactados em pacotes `.tar.zst` lidos em **uma única passada sequencial com verificação SHA-256 on-the-fly** direto para o SSD do PC (`%LOCALAPPDATA%\anvil` no Windows, `~/.local/share/anvil` no Linux).
- **Zero Visual Studio Build Tools no Windows:** combina `llvm-mingw` (UCRT x86_64) + Rust `x86_64-pc-windows-gnullvm` linkando via `clang`/`lld` — C, C++ e Rust 100% standalone.
- **Shims multicall isolados:** um único executável vinculado em `ANVIL_HOME/bin/` (`clang`, `clang++`, `clangd`, `clang-tidy`, `clang-format`, `lld`, `lldb`, `rustc`, `cargo`, `rustfmt`, `cargo-clippy`, `rust-analyzer`, `cmake`, `ninja`, `pkg-config`, `gdb`) que configura automaticamente `PATH`, `CC`, `CXX`, `RUSTC`, `RUST_SRC_PATH`, `CARGO_HOME` e `CMAKE_GENERATOR=Ninja`, inclusive para **apps GUI** que não leem `.bashrc`.
- **Auto-configuração segura de agentes (CLI + GUI):** registra o MCP `anvil mcp` com caminho absoluto e sincroniza as skills no formato nativo de cada agente, preservando chaves, outros MCPs e comentários, com backup automático antes de editar.
- **Servidor MCP unificado com Ponte LSP embutida (`anvil mcp`):** expõe 11 tools enxutas em JSON-RPC 2.0 sobre `stdio` (compilação estruturada, diagnósticos, hover, go-to-definition, referências, símbolos e leitura de skills) — assim até modelos pequenos que não dominam LSP bruto usam `clangd` e `rust-analyzer` facilmente.
- **Modo offline + atualizador sob demanda (`anvil bundle sync`):** monta ou atualiza o pendrive quando houver internet e valida toda a integridade com `anvil bundle verify`.

### O que NÃO entra neste repositório
- **Nenhum binário de terceiros é hospedado neste Git.** O repositório contém apenas o código-fonte Rust do Anvil, as skills em Markdown, os templates do USB e a receita JSON com links para os releases oficiais upstream.
- Não substitui gerenciadores de pacotes de sistema para bibliotecas gráficas/SO arbitrárias; o foco é a stack nativa **C, C++, LLVM/Clang, Rust, CMake, Ninja, LLDB/GDB, LSPs, MCP e Skills**.
- Suporte atual: **Windows x86_64** e **Linux x86_64** (sem macOS e sem ARM64 nesta versão).

---

## 🏗️ Arquitetura

```mermaid
flowchart LR
    subgraph USB["Pendrive USB (100% Offline)"]
        BIN["anvil.exe / anvil (~4 MB)"]
        DOC["AGENTS.md + START_HERE.md"]
        SK["skills/*.md"]
        MAN["manifest.json (SHA-256)"]
        BUND["bundles/*.tar.zst"]
    end

    subgraph PC["SSD Interno (Sem Admin/Sudo)"]
        SHIM["ANVIL_HOME/bin/ (shims multicall)"]
        TC["toolchains/ (llvm, rust, build-tools)"]
        MCP["anvil mcp (JSON-RPC stdio)"]
        LSP["Ponte LSP (rust-analyzer + clangd)"]
        SHIM --> TC
        MCP --> TC
        MCP --> LSP
    end

    subgraph AGENTS["Agentes Auto-Configurados"]
        SF["SiliconFlower (GUI/CLI)"]
        CC["Claude Code / Desktop"]
        CX["Codex (CLI/GUI)"]
        AG["Antigravity (CLI/GUI)"]
        OTH["Qualquer Modelo / Cliente MCP"]
    end

    BIN -->|"anvil install --yes --json"| TC
    SK -->|"emissor dual (SKILL.md / flat .md)"| AGENTS
    MCP <-->|"MCP stdio (caminho absoluto)"| AGENTS
    SHIM -.->|"HKCU Path / environment.d"| AGENTS
```

### Estrutura do Workspace Rust

```text
crates/
├── anvil-core/     # Manifest, State, Platform, Paths, EnvMode sandbox, extrator streaming .tar.zst + SHA-256
├── anvil-env/      # Dispatch multicall, shims, PATH (Win Registry / Linux environment.d+profile), doctor + smoke-tests
├── anvil-agents/   # Adaptadores SiliconFlower, Claude, Codex e Antigravity + emissor dual de skills + templates USB
├── anvil-lsp/      # Cliente LSP (Content-Length JSON-RPC) com pool lazy para rust-analyzer e clangd (1-based)
├── anvil-mcp/      # Servidor MCP stdio com wrappers estruturados Cargo/Clang/CMake e ponte LSP
├── anvil-bundle/   # Construtor de pendrive (init-usb, pack, sync com receitas JSON, verify SHA-256)
└── anvil-cli/      # Binário único `anvil` integrando dispatch multicall e todos os subcomandos
skills/             # 5 skills canônicas em Markdown (embutidas no binário via include_str!)
templates/          # AGENTS.md, START_HERE.md, install.ps1, install.sh para a raiz do USB
```

---

## 🤖 Agentes Suportados

| Agente | Config MCP (caminho absoluto) | Skills | Notas |
| :--- | :--- | :--- | :--- |
| **SiliconFlower** (GUI + CLI) | `~/.siliconflower/config.json` (ou `$SILICONFLOWER_DATA_DIR/config.json`) | Flat `.md` em `~/.siliconflower/skills/<nome>.md` | Preserva `apiKey`, `baseURL`, `hooks` e outros MCPs; caminho absoluto garante funcionamento no app Electron. |
| **Claude Code / Desktop** | `~/.claude.json` + `claude_desktop_config.json` | Pasta em `~/.claude/skills/<nome>/SKILL.md` | Desktop só é tocado se já existir (ou com `--all`). |
| **Codex** (CLI + GUI) | `~/.codex/config.toml` (`[mcp_servers.anvil]`) | Pasta em `~/.codex/skills/<nome>/SKILL.md` (+ `~/.agents/skills/` se existir) | Edição via `toml_edit` preservando comentários e formatação. |
| **Antigravity** (CLI + GUI) | `~/.gemini/antigravity/mcp_config.json` | Pasta em `~/.gemini/antigravity/skills/<nome>/SKILL.md` | Compatível com descoberta nativa de skills. |
| **Qualquer outro cliente / modelo aberto** | `anvil agents print-config --format json\|toml` | `anvil skills list/read` ou tools MCP `skill_list`/`skill_read` | Basta ler `AGENTS.md` na raiz do USB. |

> **Segurança:** toda modificação em config existente gera backup automático em `ANVIL_HOME/backups/`. Skills gravadas pelo Anvil carregam um marcador gerenciado; skills escritas por você nunca são sobrescritas nem apagadas no `uninstall`.

---

## 🧰 Servidor MCP Unificado (`anvil mcp`)

Todos os números de linha e coluna são **1-based** (como em compiladores e editores) para evitar erros off-by-one em LLMs.

| Tool | Descrição |
| :--- | :--- |
| `anvil_doctor` | Verifica toolchains, versões, agentes e (opcional) compila/executa smoke-tests em C, C++ e Rust. |
| `rust_cargo` | Roda `check`, `build`, `test`, `clippy`, `fmt_check` ou `fmt` com diagnósticos JSON estruturados (`file`, `line`, `col`, `code`, `message`). |
| `cc_build` | Configura e compila C/C++ com CMake + Ninja (gerando `compile_commands.json` para o `clangd`) ou compila arquivo único direto com Clang. |
| `cc_lint_format` | Roda `clang-format` ou `clang-tidy` em modo `check` ou `fix`. |
| `lsp_diagnostics` | Diagnósticos em tempo real via `rust-analyzer` ou `clangd`. |
| `lsp_hover` | Tipo, assinatura e documentação em `(file, line, col)`. |
| `lsp_definition` | Ir para definição em `(file, line, col)`. |
| `lsp_references` | Listar referências do símbolo em `(file, line, col)`. |
| `lsp_symbols` | Listar símbolos do arquivo (`file`) ou buscar no workspace (`query`). |
| `skill_list` | Lista as 5 skills embutidas. |
| `skill_read` | Lê o conteúdo completo de uma skill (`anvil-toolkit`, `rust-systems`, `c-modern`, `cpp-modern`, `cmake-ninja-debug`). |

---

## 🚀 Uso Rápido

### 1. Montar ou atualizar o pendrive (PC com internet)

```powershell
# Compilar o binário release
cargo build --release --bin anvil

# Inicializar a raiz do pendrive (copia AGENTS.md, START_HERE.md, scripts, skills e binários)
.\target\release\anvil.exe bundle init-usb --usb E:\anvil --win-exe .\target\release\anvil.exe

# Conferir o plano de download dos toolchains oficiais (dry-run)
.\target\release\anvil.exe bundle sync --usb E:\anvil --targets windows-x86_64,linux-x86_64 --dry-run

# Baixar e empacotar tudo em .tar.zst no pendrive (resumível com cache local)
.\target\release\anvil.exe bundle sync --usb E:\anvil --targets windows-x86_64,linux-x86_64

# Validar SHA-256, tamanhos e arquivos do pendrive inteiro
.\target\release\anvil.exe bundle verify --usb E:\anvil --json
```

> **Dicas:**
> - Para personalizar versões/URLs sem recompilar, exporte a receita padrão com `anvil bundle print-recipe > recipe.json`, edite e passe `--recipe recipe.json`.
> - Para empacotar manualmente qualquer pasta de toolchain local: `anvil bundle pack --usb E:\anvil --platform windows-x86_64 --id llvm --version 20.1 --source C:\caminho\llvm`.
> - Baixe o binário Linux (`anvil-linux-x86_64`) nos artefatos da aba **Actions** do GitHub e salve como `E:\anvil\anvil` na raiz do pendrive.

### 2. Instalar em um PC recém-formatado (100% offline)

**Windows (PowerShell):**
```powershell
.\anvil.exe install --yes --json
& "$env:LOCALAPPDATA\anvil\bin\anvil.exe" doctor --smoke-test --json
```

**Linux:**
```bash
sh ./install.sh --json
~/.local/share/anvil/bin/anvil doctor --smoke-test --json
```

Qualquer agente de IA que abrir a pasta do pendrive e ler [AGENTS.md](templates/AGENTS.md) executa exatamente esse fluxo sozinho.

---

## 💻 Referência da CLI

```text
anvil install         Instala bundles .tar.zst em streaming, cria shims, configura PATH e agentes
anvil doctor          Verifica instalação, versões, agentes e (--smoke-test) compila/roda C, C++ e Rust
anvil env             Imprime comandos de shell (powershell, bash, zsh, fish, json) para a sessão atual
anvil agents          detect | setup | remove | print-config
anvil skills          list | read <nome> | sync
anvil mcp             Sobe o servidor MCP unificado sobre stdio (toolchains + ponte LSP + skills)
anvil bundle          init-usb | pack | sync | verify | print-recipe
anvil uninstall       Remove instalação, shims, PATH e entradas do Anvil nos agentes
```

Todos os comandos de automação aceitam `--json` (stdout sempre contém **exatamente um documento JSON**; progresso e avisos vão para `stderr`).

---

## 🧪 Desenvolvimento e CI

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --release --bin anvil
```

Os testes nunca alteram o Registry do Windows nem o `$HOME` real — toda a suíte roda isolada via `ANVIL_ENV_SANDBOX`.
O workflow [.github/workflows/ci.yml](.github/workflows/ci.yml) compila, testa e publica os binários de **Windows x86_64** e **Linux x86_64** a cada push/PR.

---

## 📄 Licença e Componentes de Terceiros

O código-fonte do **Anvil** é distribuído sob a licença **[MIT](LICENSE)**.

Os toolchains baixados localmente pelo comando `anvil bundle sync` pertencem aos seus respectivos projetos upstream e usam licenças open-source permissivas:

| Componente Upstream | Licença |
| :--- | :--- |
| LLVM / Clang / clangd / lld / lldb | Apache-2.0 WITH LLVM-exception |
| llvm-mingw | ISC / Permissive |
| Rust / Cargo / Clippy / rustfmt / rust-analyzer | MIT OR Apache-2.0 |
| CMake | BSD-3-Clause |
| Ninja | Apache-2.0 |
