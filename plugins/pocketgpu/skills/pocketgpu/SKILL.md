---
name: pocketgpu
description: Use on a computer that has PocketGPU (the pgpu command) before GPU work, to run long jobs, and to tell the user's phone when a long task is done or needs them.
---

# PocketGPU

PocketGPU shows this computer's GPUs and jobs on the user's phone. Its tools are `machine_status`, `list_jobs`, `job_output` and `notify_phone`.

## Before GPU work

Call `machine_status` first. It says whether the GPU is free (memory in use, load) and what else is running.

## Long jobs

Start anything that may take more than a few minutes as a watched job, in the background, so your own command time limit can't stop it:

    nohup pgpu run --name <short name> -- <command> < /dev/null > /dev/null 2>&1 &

Or use your own way of running a command in the background (on Windows, `Start-Process`). The phone gets an alert when the job ends. Follow it with `list_jobs` and `job_output` instead of waiting at the terminal.

## Notes to the phone

Call `notify_phone` once when a long task finishes, fails or needs a decision from the user. Not for progress. At most 10 an hour. Say in a few words what happened and what's next.

Never put secrets, keys, passwords, file contents or personal data in a note.

## When PocketGPU isn't there

If the tools fail or `pgpu` isn't installed, say so and carry on without it. Don't install anything.
