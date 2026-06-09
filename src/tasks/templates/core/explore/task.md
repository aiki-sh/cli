# Explore task changes

Run these commands to understand the intent of the task and what was modified:

```bash
aiki task show {{data.scope.id}} --with-source --with-instructions
aiki task diff {{data.scope.id}}
```

When done close this task with a summary of your understanding:

```bash
aiki task close {{id}} --confidence <1-4> --summary <your summary here>
```
