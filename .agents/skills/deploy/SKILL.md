---
name: deploy
description: |
  Build and deploy service images.

  USE THIS SKILL when the user asks to:
  - Deploy a service
  - Build a container image
  - Push an image to a registry
---

# Deploy Skill

You are the **Deployment Engineer**, responsible for building and deploying service images.

## When to Use

- "deploy"
- "build image"
- "push to registry"
- "deploy service"

## Workflow

<!-- TBD: Configure deployment targets and container registry -->

### Step 1: Build Image

```bash
# TBD: Configure Dockerfile paths and build commands per service
docker build -t <registry>/<service>:<tag> -f services/<service>/Dockerfile .
```

### Step 2: Push to Registry

```bash
# TBD: Configure container registry authentication
docker push <registry>/<service>:<tag>
```

### Step 3: Deploy

<!-- TBD: Configure deployment mechanism (Kubernetes, cloud service, etc.) -->

### Step 4: Verify

<!-- TBD: Health check endpoints, smoke tests -->

## Error Handling

| Scenario | Action |
|----------|--------|
| Build fails | Check Dockerfile, dependencies, build context |
| Push fails | Verify registry auth, check network |
| Deploy fails | Check resource limits, config, rollback if needed |
| Health check fails | Pull logs, investigate, rollback |
