# Session checkpoint

> Optional prompt for the current host-session checkpoint. Rewrite it as the
> current state, not a log. Aim for about 2 KiB; the hard limit is 256 KiB.
> Do not record live lanes, PR status or progress history here: the digest
> derives lanes and PRs (write PRs as `PR #N`), and `provenant chair log`
> holds the progress history.

## Goal

<What this session is for, the outcome that closes it, and the issue, PR or
project document that owns the durable record.>

## Direction

<User rulings and boundaries that govern the work, newest first, each with its
date and the owner that records it. Note any gate that needs the user.>

## Process

<How this session works: the skills and gates in use, where harness scratch
lives, where durable results go, and which checks run in CI.>

## Plan

<Ordered next steps within authority. End with the resume trigger: the event
or instruction that resumes the work, and the run, task or PR to reconcile
first.>
