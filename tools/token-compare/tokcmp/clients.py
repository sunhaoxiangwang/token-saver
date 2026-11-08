"""Client launchers. One process per attempt, fresh fixture, fresh relay directory, no session
reuse. Commands are built from the pinned configuration; every value the client reports about
itself (version, model) is taken from its own output at extraction time."""
import json
import os
import secrets
import signal
import subprocess
import time

TOKEN_SAVER_TOOLS = ["workspace_outline", "file_slice", "text_search"]
CLAUDE_MCP_TOOLS = ["mcp__token-saver__" + t for t in TOKEN_SAVER_TOOLS]
CLAUDE_NATIVE_TOOLS = ["Read", "Grep", "Glob", "Bash", "LS"]
CLAUDE_DISALLOWED_IN_B = ["Read", "Grep", "Glob", "Bash", "LS", "Edit", "Write", "MultiEdit",
                          "NotebookEdit", "WebFetch", "WebSearch", "Task", "Agent"]

ARM_B_INSTRUCTION = ("Use only the token-saver MCP tools (workspace_outline, file_slice, "
                     "text_search) to inspect the project. Do not run shell commands and do not "
                     "read files by any other means. ")


def new_nonce():
    return secrets.token_hex(8)


def prompt_for(arm, task_prompt):
    if arm == "B":
        return ARM_B_INSTRUCTION + task_prompt
    return task_prompt


def relay_server_args(cfg, nonce, relay_dir, fixture):
    return [cfg["relay_bin"], "--expect-nonce", nonce, "--out", relay_dir, "--",
            cfg["server_bin"], "serve", "--diag", "info", "--root", fixture]


def write_claude_config(arm, attempt_dir, cfg, nonce, relay_dir, fixture):
    mcp = {"mcpServers": {}}
    if arm in ("B", "C"):
        argv = relay_server_args(cfg, nonce, relay_dir, fixture)
        mcp["mcpServers"]["token-saver"] = {"type": "stdio", "command": argv[0], "args": argv[1:]}
    mcp_path = os.path.join(attempt_dir, "mcp.json")
    with open(mcp_path, "w") as f:
        json.dump(mcp, f)
    hook_out = os.path.join(attempt_dir, "hook.jsonl")
    settings = {"hooks": {"PostToolUse": [{
        "matcher": "mcp__token-saver__.*",
        "hooks": [{"type": "command",
                   "command": "python3 -I %s %s %s" % (cfg["hook_extract"], hook_out, nonce)}]}]}}
    settings_path = os.path.join(attempt_dir, "settings.json")
    with open(settings_path, "w") as f:
        json.dump(settings, f)
    return mcp_path, settings_path, hook_out


def claude_command(arm, task_prompt, attempt_dir, cfg, nonce, relay_dir, fixture):
    mcp_path, settings_path, _ = write_claude_config(arm, attempt_dir, cfg, nonce, relay_dir, fixture)
    c = cfg["claude"]
    cmd = ["env", "-u", "CLAUDECODE", c.get("bin", "claude"), "-p",
           "--model", c["model"], "--strict-mcp-config", "--mcp-config", mcp_path,
           "--settings", settings_path, "--output-format", "stream-json", "--verbose"]
    if arm == "A":
        allowed = CLAUDE_NATIVE_TOOLS + ["ToolSearch"]
        disallowed = []
    elif arm == "B":
        allowed = CLAUDE_MCP_TOOLS + ["ToolSearch"]
        disallowed = CLAUDE_DISALLOWED_IN_B
    else:
        allowed = CLAUDE_NATIVE_TOOLS + CLAUDE_MCP_TOOLS + ["ToolSearch"]
        disallowed = []
    # `--allowedTools` and `--disallowedTools` are variadic: a fixed-arity flag must separate
    # them from the prompt, otherwise the prompt is read as a tool name.
    cmd += ["--allowedTools", ",".join(allowed)]
    if disallowed:
        cmd += ["--disallowedTools", ",".join(disallowed)]
    cmd += ["--max-turns", str(c["max_turns"])]
    cmd.append(prompt_for(arm, task_prompt))
    return cmd


def codex_command(arm, task_prompt, attempt_dir, cfg, nonce, relay_dir, fixture):
    c = cfg["codex"]
    cmd = [c.get("bin", "codex"), "exec", "--ignore-user-config", "--ephemeral",
           "--skip-git-repo-check", "-C", fixture, "--json",
           "-c", 'model="%s"' % c["model"],
           "-c", 'model_reasoning_effort="%s"' % c["reasoning_effort"],
           "-c", 'sandbox_mode="read-only"',
           "-c", 'approval_policy="never"']
    if arm in ("B", "C"):
        argv = relay_server_args(cfg, nonce, relay_dir, fixture)
        cmd += ["-c", 'mcp_servers.token-saver.command="%s"' % argv[0],
                "-c", "mcp_servers.token-saver.args=" + json.dumps(argv[1:])]
    cmd.append(prompt_for(arm, task_prompt))
    return cmd


def run_command(cmd, cwd, stdout_path, stderr_path, timeout_s, env_extra=None):
    """Runs the client with stdin closed, capturing stdout and stderr to files. Returns a dict
    with exit code, wall time, and a timeout flag. On timeout the process group is killed."""
    env = dict(os.environ)
    if env_extra:
        env.update(env_extra)
    started = time.time()
    with open(stdout_path, "wb") as out, open(stderr_path, "wb") as err, open(os.devnull, "rb") as devnull:
        proc = subprocess.Popen(cmd, cwd=cwd, stdin=devnull, stdout=out, stderr=err, env=env,
                                start_new_session=True)
        timed_out = False
        try:
            proc.wait(timeout=timeout_s)
        except subprocess.TimeoutExpired:
            timed_out = True
            try:
                os.killpg(proc.pid, signal.SIGTERM)
                proc.wait(timeout=5)
            except Exception:
                try:
                    os.killpg(proc.pid, signal.SIGKILL)
                except Exception:
                    pass
                proc.wait()
    return {"exit_code": proc.returncode, "wall_ms": int((time.time() - started) * 1000),
            "timed_out": timed_out, "started_unix_ms": int(started * 1000)}
