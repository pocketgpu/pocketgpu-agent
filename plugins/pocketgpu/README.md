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

## Answering from your phone

The plugin also lets your phone answer Claude Code, once you turn it on with
`pgpu assistants set ask-on-phone on` or `pgpu assistants set ask-first delete git-push`
(or from the app; answering on the phone needs PocketGPU Pro):

- **Ask on my phone**: when Claude Code would ask you at the computer, your phone can Allow
  or Deny too. The first answer counts.
- **Ask me first**: chosen kinds of action (deleting files, `git push`, stopping programs,
  installing packages, your own words) wait for your phone even in auto mode. No answer in
  5 minutes, and they don't run.

"Ask me first" needs **Claude Code 2.1.295 or newer**: older versions let an action go ahead
when a hook fails, so PocketGPU won't turn it on for them. It is a safety net, not a wall: an
assistant with a terminal can get around it.

The plugin brings "Ask on my phone". "Ask me first" stops an action when its hook can't run,
so the plugin doesn't carry it: `pgpu assistants connect` adds that hook with `pgpu`'s full
path, and `pgpu assistants disconnect` takes it away again.
