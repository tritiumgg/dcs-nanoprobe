# The nanoprobe skill

`SKILL.md` is an [Agent Skill](https://agentskills.io/specification): a folder with a
`SKILL.md` whose frontmatter holds `name` and `description`. AI coding tools that
support the format load it when a task is about nanoprobe. It points at the guides and
the schema and does not copy them.

## Install

Copy the folder `skills/nanoprobe/` to the skills folder of your tool, keeping the
folder name `nanoprobe`. A project folder applies to one repository, such as the one
that holds your controller; a user folder applies everywhere.

| Tool | Project folder | User folder |
|---|---|---|
| Claude Code | `.claude/skills/nanoprobe/` | `~/.claude/skills/nanoprobe/` |
| OpenAI Codex | `.agents/skills/nanoprobe/` | `~/.agents/skills/nanoprobe/` |
| Gemini CLI | `.agents/skills/nanoprobe/` or `.gemini/skills/nanoprobe/` | `~/.agents/skills/nanoprobe/` or `~/.gemini/skills/nanoprobe/` |
| Cursor | `.agents/skills/nanoprobe/` or `.cursor/skills/nanoprobe/` | `~/.agents/skills/nanoprobe/` or `~/.cursor/skills/nanoprobe/` |
| GitHub Copilot in VS Code | `.github/skills/nanoprobe/` | `~/.copilot/skills/nanoprobe/` |
| GitHub Copilot CLI and cloud agent | `.github/skills/nanoprobe/` or `.agents/skills/nanoprobe/` | `~/.copilot/skills/nanoprobe/` or `~/.agents/skills/nanoprobe/` |

`.agents/skills/` is read by Codex, Gemini CLI and Cursor, and by the Copilot CLI and
cloud agent. Claude Code reads `.claude/skills/`. For both, copy the folder to
`.agents/skills/nanoprobe/` and link `.claude/skills/nanoprobe` to it. Where you cannot
make a symbolic link, as on Windows without Developer Mode, copy the folder to both.

Each tool documents its own locations:

- Claude Code: <https://code.claude.com/docs/en/skills>
- OpenAI Codex: <https://developers.openai.com/codex/skills>
- Gemini CLI: <https://geminicli.com/docs/cli/skills/>
- Cursor: <https://cursor.com/docs/context/skills>
- GitHub Copilot in VS Code:
  <https://code.visualstudio.com/docs/copilot/customization/agent-skills>
- GitHub Copilot CLI and cloud agent:
  <https://docs.github.com/en/copilot/concepts/agents/about-agent-skills>

Other tools that support the format are listed at <https://agentskills.io/clients>.

## Versions

The skill links the guides on the `main` branch. Generate your client from the schema
of the release your server runs; the guides say where to find it.
