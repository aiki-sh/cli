# Base image for the aiki harness capture rig.
# linux/arm64 on Apple Silicon via `container build`. One harness CLI per image.
#
#   container build -t aiki-rig-claude --build-arg HARNESS=claude -f Dockerfile .
#
# The same file builds with podman/docker. Credentials are NOT baked in; they are
# mounted read-only at run time by capture.sh.
ARG NODE=22
FROM node:${NODE}-bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates git jq curl bash \
    && rm -rf /var/lib/apt/lists/*

# Per-container HOME is the isolation unit. The image runs as the base image's
# non-root `node` user, because Claude Code (and others) refuse
# --dangerously-skip-permissions when running as root.
ENV HOME=/home/node

# Universal capture shim: cats a hook's stdin to /out/<harness>/<event>.jsonl.
COPY logging-hook.sh /usr/local/bin/aiki-capture-hook
RUN chmod +x /usr/local/bin/aiki-capture-hook

# Install exactly one harness CLI at build time (kept warm in the image).
ARG HARNESS
COPY harnesses/${HARNESS}.sh /harness.sh
RUN bash -lc 'source /harness.sh && harness_install'

# Writable work/output + pre-created node-owned config dirs, so a read-only
# credential file-mount overlays only the file and the dir stays writable.
RUN mkdir -p /work /out /home/node/.claude /home/node/.codex \
    && chown -R node:node /home/node /work /out
WORKDIR /work
USER node

# capture.sh passes the wire-hooks + run-prompt command as the CMD string.
ENTRYPOINT ["/bin/bash", "-lc"]
