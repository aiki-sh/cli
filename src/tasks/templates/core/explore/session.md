# Explore session changes

Examine each task completed in this session. The task IDs are: `{{data.scope.task_ids}}`

For each task, run:

```bash
aiki task show <task-id> --with-source --with-instructions
aiki task diff <task-id>
```

When done close this task with a summary of your understanding:

```bash
aiki task close {{id}} --confidence <1-4> --summary <your summary here>
```
