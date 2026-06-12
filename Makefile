# memd installer
#
# One command installs memd end to end: binary, skill, and enforcement wiring.
# Uninstall mirrors it while keeping ~/.memd memory data intact.
# Modeled on ../omics-skills/Makefile (symlink-by-default = always up to date).
# No assumptions about any private meta-repo: everything targets standard paths.
#
# Staleness problem this solves: memd ends up copied to ~/.local/bin and the
# skill vendored into several places, and those copies drift. Here the repo is
# the single source of truth; symlinks keep the installed copies current.

.PHONY: help build install install-prebuilt install-binary install-skill \
        install-skill-bundle install-skill-copy install-enforcement install-all \
        menu uninstall uninstall-binary uninstall-skill uninstall-enforcement \
        status clean

REPO          := $(CURDIR)
RELEASE_BIN   := $(REPO)/target/release/memd
INSTALLER_URL := https://github.com/fmschulz/memd/releases/latest/download/memd-installer.sh

PREFIX  ?= $(HOME)/.local
BIN_DIR := $(PREFIX)/bin
BIN     := $(BIN_DIR)/memd

# symlink (default: installed copies track the repo) or copy (standalone).
INSTALL_METHOD ?= symlink

# Skill source + the three standard skill directories. ~/.agents/skills is the
# shared canonical location; ~/.claude/skills and ~/.codex/skills are where
# Claude Code and Codex look (often themselves symlinks to ~/.agents/skills).
SKILL_NAME    := memd
SKILL_SRC     := $(REPO)/memd-skill
SKILL_BIN_REL := bin/linux-x64/memd
AGENTS_SKILLS := $(HOME)/.agents/skills
CLAUDE_SKILLS := $(HOME)/.claude/skills
CODEX_SKILLS  := $(HOME)/.codex/skills

# Colors (disabled if NO_COLOR is set or stdout is not a TTY).
ifeq ($(NO_COLOR),)
  ifeq ($(shell test -t 1 && echo 1),1)
    GREEN  := \033[0;32m
    YELLOW := \033[0;33m
    BLUE   := \033[0;34m
    RED    := \033[0;31m
    NC     := \033[0m
  endif
endif

##@ General

help: ## Display this help
	@echo "$(BLUE)memd installer$(NC)"
	@echo ""
	@echo "Usage: make <target> [INSTALL_METHOD=copy] [PREFIX=$(HOME)/.local]"
	@echo ""
	@awk 'BEGIN {FS = ":.*##"} \
		/^[a-zA-Z_-]+:.*?##/ { printf "  $(GREEN)%-18s$(NC) %s\n", $$1, $$2 } \
		/^##@/ { printf "\n$(YELLOW)%s$(NC)\n", substr($$0, 5) }' $(MAKEFILE_LIST)

menu: ## Interactive TUI to select components to install or uninstall
	@python3 "$(REPO)/scripts/install_tui.py" --repo "$(REPO)" \
		--make-program "$(MAKE)" --install-method "$(INSTALL_METHOD)"

##@ Install

build: ## Build the release binary (cargo build --release -p memd)
	@if ! command -v cargo >/dev/null 2>&1; then \
		echo "cargo not found — install Rust (https://rustup.rs), or skip compiling entirely:"; \
		echo "  make install-prebuilt"; \
		exit 1; \
	fi
	@echo "$(BLUE)Building memd (release)...$(NC)"
	@cargo build --release -p memd

install: build install-binary install-skill install-enforcement ## Install everything: binary + skill + enforcement (idempotent)
	@$(MAKE) --no-print-directory status
	@echo "$(GREEN)✓ memd fully installed (binary + skill + enforcement) — idempotent, safe to re-run$(NC)"
	@echo "  next: run 'memd doctor --strict' to verify (exit 0 on a healthy fresh install)"

install-prebuilt: ## Install everything WITHOUT compiling: prebuilt release binary if it works here, else build; + skill + enforcement
	@ok=0; \
	if command -v curl >/dev/null 2>&1; then \
		echo "$(BLUE)Installing the prebuilt binary from the latest GitHub release...$(NC)"; \
		command -v memd >/dev/null 2>&1 && memd warm stop >/dev/null 2>&1 || true; \
		curl --proto '=https' --tlsv1.2 -LsSf "$(INSTALLER_URL)" | sh >/dev/null 2>&1 || true; \
		if [ -x "$(BIN)" ] && "$(BIN)" --version >/dev/null 2>&1; then ok=1; fi; \
	else \
		echo "$(YELLOW)curl not found — skipping the prebuilt path$(NC)"; \
	fi; \
	if [ "$$ok" = "1" ]; then \
		echo "  $(GREEN)✓$(NC) prebuilt binary works: $$("$(BIN)" --version) ($(BIN))"; \
	else \
		echo "  $(YELLOW)prebuilt unavailable or does not run on this platform — building from source$(NC)"; \
		$(MAKE) --no-print-directory install-binary; \
	fi
	@$(MAKE) --no-print-directory install-skill install-enforcement status
	@echo "$(GREEN)✓ memd installed without compiling (prebuilt-first) — idempotent, safe to re-run$(NC)"
	@echo "  next: run 'memd doctor --strict' to verify"

install-binary: build ## Binary only (escape hatch): build + install memd onto PATH
	@if [ ! -x "$(RELEASE_BIN)" ]; then \
		echo "$(RED)✗$(NC) $(RELEASE_BIN) not found — run 'make build'"; exit 1; fi
	@mkdir -p $(BIN_DIR)
	@command -v memd >/dev/null 2>&1 && memd warm stop >/dev/null 2>&1 || true
	@if [ "$(INSTALL_METHOD)" = "copy" ]; then \
		tmp="$$(mktemp "$(BIN).tmp.XXXXXX")"; cp "$(RELEASE_BIN)" "$$tmp"; chmod 0755 "$$tmp"; mv -f "$$tmp" "$(BIN)"; \
		echo "  $(GREEN)✓$(NC) copied to $(BIN)"; \
	else \
		ln -sfn "$(RELEASE_BIN)" "$(BIN)"; \
		echo "  $(GREEN)✓$(NC) $(BIN) -> $(RELEASE_BIN)"; \
	fi
	@hash -r 2>/dev/null || true
	@echo "  $(GREEN)✓$(NC) $$("$(BIN)" --version)"
	@case ":$$PATH:" in *":$(BIN_DIR):"*) ;; *) echo "  $(YELLOW)⚠ $(BIN_DIR) is not on PATH — add 'export PATH=\"$(BIN_DIR):$$PATH\"' to your shell rc$(NC)";; esac

install-skill: ## Install the skill into ~/.agents, ~/.claude, ~/.codex skills
	@mkdir -p "$(AGENTS_SKILLS)"
	@target="$(AGENTS_SKILLS)/$(SKILL_NAME)"; \
	if [ -L "$$target" ]; then rm -f "$$target"; \
	elif [ -e "$$target" ]; then mv "$$target" "$$target.bak.$$(date +%s)"; echo "  $(YELLOW)backed up real $$target$(NC)"; fi; \
	if [ "$(INSTALL_METHOD)" = "copy" ]; then cp -r "$(SKILL_SRC)" "$$target"; else ln -sfn "$(SKILL_SRC)" "$$target"; fi; \
	echo "  $(GREEN)✓$(NC) $$target -> $(SKILL_SRC)"
	@for d in "$(CLAUDE_SKILLS)" "$(CODEX_SKILLS)"; do \
		if [ "$$(readlink -f "$$d" 2>/dev/null)" = "$$(readlink -f "$(AGENTS_SKILLS)" 2>/dev/null)" ]; then \
			echo "  $(GREEN)✓$(NC) $$d/$(SKILL_NAME) (inherited: $$d -> ~/.agents/skills)"; \
		else \
			mkdir -p "$$d"; t="$$d/$(SKILL_NAME)"; \
			if [ -L "$$t" ]; then rm -f "$$t"; \
			elif [ -e "$$t" ]; then mv "$$t" "$$t.bak.$$(date +%s)"; echo "  $(YELLOW)backed up real $$t$(NC)"; fi; \
			ln -sfn "$(AGENTS_SKILLS)/$(SKILL_NAME)" "$$t"; \
			echo "  $(GREEN)✓$(NC) $$t -> $(AGENTS_SKILLS)/$(SKILL_NAME)"; \
		fi; \
	done

install-skill-bundle: build ## Copy skill + built binary into existing skill dirs only
	@if [ ! -x "$(RELEASE_BIN)" ]; then \
		echo "$(RED)✗$(NC) $(RELEASE_BIN) not found — run 'make build' first"; exit 1; fi
	@found=0; seen=""; \
	for d in "$(AGENTS_SKILLS)" "$(CLAUDE_SKILLS)" "$(CODEX_SKILLS)"; do \
		if [ ! -d "$$d" ]; then \
			echo "  $(YELLOW)○$(NC) skipped $$d (missing)"; continue; fi; \
		real="$$(readlink -f "$$d" 2>/dev/null || printf '%s' "$$d")"; \
		case " $$seen " in *" $$real "*) \
			echo "  $(YELLOW)○$(NC) skipped $$d (same directory as an earlier skill dir)"; continue ;; \
		esac; \
		seen="$$seen $$real"; found=1; target="$$d/$(SKILL_NAME)"; \
		if [ -L "$$target" ]; then rm -f "$$target"; \
		elif [ -e "$$target" ]; then \
			backup="$$target.bak.$$(date +%s)"; mv "$$target" "$$backup"; \
			echo "  $(YELLOW)backed up real $$target to $$backup$(NC)"; \
		fi; \
		mkdir -p "$$target"; \
		cp -a "$(SKILL_SRC)/." "$$target/"; \
		mkdir -p "$$target/$$(dirname "$(SKILL_BIN_REL)")"; \
		install -m 0755 "$(RELEASE_BIN)" "$$target/$(SKILL_BIN_REL)"; \
		echo "  $(GREEN)✓$(NC) copied skill bundle to $$target"; \
		echo "  $(GREEN)✓$(NC) $$target/$(SKILL_BIN_REL): $$("$$target/$(SKILL_BIN_REL)" --version)"; \
	done; \
	if [ "$$found" = "0" ]; then \
		echo "$(RED)✗$(NC) none of $(AGENTS_SKILLS), $(CLAUDE_SKILLS), or $(CODEX_SKILLS) exists"; exit 1; \
	fi

install-skill-copy: install-skill-bundle ## Alias for install-skill-bundle

install-enforcement: ## Wire CLI-first agent rules + SessionStart hook (skill installer)
	@"$(SKILL_SRC)/install_memd_enforcement.sh"

install-all: install ## Alias of install (kept for muscle memory)

##@ Uninstall

uninstall: uninstall-binary uninstall-skill uninstall-enforcement ## Remove binary + skill + enforcement (keeps ~/.memd data)
	@echo "$(GREEN)✓ uninstalled (binary + skill + enforcement)$(NC)"
	@echo "  kept: ~/.memd (your memory data) — remove manually for a clean slate"

uninstall-binary: ## Remove the installed binary from PATH
	@command -v memd >/dev/null 2>&1 && memd warm stop >/dev/null 2>&1 || true
	@if [ -e "$(BIN)" ] || [ -L "$(BIN)" ]; then rm -f "$(BIN)"; echo "  removed $(BIN)"; \
	else echo "  $(YELLOW)○$(NC) $(BIN) not present"; fi

uninstall-skill: ## Remove the skill from ~/.agents, ~/.claude, ~/.codex skills
	@removed=0; for d in "$(AGENTS_SKILLS)" "$(CLAUDE_SKILLS)" "$(CODEX_SKILLS)"; do \
		t="$$d/$(SKILL_NAME)"; \
		if [ -L "$$t" ] || [ -e "$$t" ]; then rm -rf "$$t"; echo "  removed $$t"; removed=1; fi; \
	done; \
	[ "$$removed" = "0" ] && echo "  $(YELLOW)○$(NC) skill not installed" || true

uninstall-enforcement: ## Remove agent rule blocks, Cursor rule, and SessionStart hook
	@"$(SKILL_SRC)/uninstall_memd_enforcement.sh"

##@ Status & maintenance

status: ## Show built/installed version, PATH location, and skill links
	@echo "$(BLUE)memd install status$(NC)"
	@printf "  repo build:    "; [ -x "$(RELEASE_BIN)" ] && "$(RELEASE_BIN)" --version || echo "$(YELLOW)not built (run 'make build')$(NC)"
	@printf "  on PATH:       "; command -v memd >/dev/null 2>&1 && memd --version || echo "$(RED)memd not on PATH$(NC)"
	@printf "  PATH location: "; command -v memd 2>/dev/null || echo "-"
	@if [ -x "$(RELEASE_BIN)" ] && command -v memd >/dev/null 2>&1; then \
		rv="$$("$(RELEASE_BIN)" --version)"; pv="$$(memd --version)"; \
		if [ "$$rv" != "$$pv" ]; then echo "  $(YELLOW)⚠ PATH memd ($$pv) != repo build ($$rv) — run 'make install'$(NC)"; \
		else echo "  $(GREEN)✓ PATH memd matches the repo build$(NC)"; fi; \
	fi
	@echo "  skill ($(SKILL_NAME)):"
	@for d in "$(AGENTS_SKILLS)" "$(CLAUDE_SKILLS)" "$(CODEX_SKILLS)"; do \
		t="$$d/$(SKILL_NAME)"; printf "    %-28s " "$$t:"; \
		if [ -L "$$t" ]; then echo "-> $$(readlink "$$t")"; \
		elif [ -d "$$t" ]; then echo "(copy)"; \
		else echo "-"; fi; \
	done
	@printf "  enforcement:   "; if grep -qs "memd-enforcement:start" "$(HOME)/.claude/CLAUDE.md"; then echo "$(GREEN)wired$(NC)"; else echo "$(YELLOW)not wired (run 'make install-enforcement')$(NC)"; fi
	@printf "  SessionStart:  "; if grep -qs "memd session-start" "$(HOME)/.claude/settings.json"; then echo "$(GREEN)wired$(NC)"; else echo "$(YELLOW)not wired$(NC)"; fi
	@printf "  warm worker:   "; if command -v memd >/dev/null 2>&1; then \
		if RUST_LOG=error memd warm status 2>/dev/null | grep -q '"status": "running"'; then echo "running"; else echo "stopped"; fi; \
	else echo "- (memd not on PATH)"; fi

clean: ## Remove skill .bak backups created by install-skill
	@find "$(AGENTS_SKILLS)" "$(CLAUDE_SKILLS)" "$(CODEX_SKILLS)" -maxdepth 1 -name '$(SKILL_NAME).bak.*' -exec rm -rf {} + 2>/dev/null || true
	@echo "$(GREEN)✓ removed skill backups$(NC)"
