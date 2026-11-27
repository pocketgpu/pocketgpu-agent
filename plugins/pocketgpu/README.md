# PocketGPU for Claude Code

Your GPU computer on your phone, with Claude Code in the loop. This plugin gives Claude Code
PocketGPU's tools (GPU status, jobs and their output, a short note to your phone), a skill
that says when to use them, and hooks that ping your phone when Claude Code needs you or
finishes a long task. The pings carry the assistant's and the computer's names only, never
what Claude wrote.

It needs the PocketGPU agent on the same computer, with `pgpu` on your PATH:
see [pocketgpu.app](https://pocketgpu.app).

```sh
claude plugin marketplace add pocketgpu/pocketgpu-agent
claude plugin install pocketgpu@pocketgpu
```

Without the plugin, `pgpu assistants connect` sets up the same for Claude Code, Codex and
Cursor.
