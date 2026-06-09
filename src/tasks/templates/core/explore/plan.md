# Understand the plan

Read the plan file at {{data.scope.id}} to understand the design.

If this plan was created from a task, check the task for additional context:

```bash
aiki task list --source file:{{data.scope.id}}
```

When done close this task with a summary of your understanding:

```bash
aiki task close {{id}} --confidence <1-4> --summary <your summary here>
```
