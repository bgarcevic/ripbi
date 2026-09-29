"""Run an Azure Pipelines steps template locally, for templates/ripbi-scan.yml.

Azure Pipelines cannot run on a GitHub runner, so the e2e workflow runs the
template under this emulator instead. It knows just enough of the agent to run
that file faithfully:

- template expressions: ``${{ parameters.NAME }}`` inside strings, and a
  ``- ${{ if parameters.NAME }}:`` item inserting its steps into a sequence;
- macros: ``$(Name)`` in ``env``, ``inputs``, and ``workingDirectory``, left
  as written when the variable does not exist, as the agent leaves them;
- ``bash`` steps, run as the agent runs them (a script file under
  ``bash --noprofile --norc``), with every variable in the environment under
  its agent name (``Agent.TempDirectory`` as ``AGENT_TEMPDIRECTORY``);
- the logging commands ``task.setvariable`` (``isoutput=true`` makes it
  ``<step name>.<variable>``), ``task.prependpath``, ``task.logissue``, and
  ``task.uploadsummary``;
- conditions made of ``succeeded()``, ``failed()``, ``always()``,
  ``succeededOrFailed()``, ``and``, ``or``, ``not``, ``eq``, ``ne``,
  ``variables['NAME']``, and string literals;
- ``continueOnError`` and ``PublishBuildArtifacts@1``, which checks its path
  and records the artifact.

Anything else is an error, so the template cannot quietly outgrow it. Needs
PyYAML. Writes the run's result as JSON:

    python3 scripts/run_ado_template.py templates/ripbi-scan.yml \\
      --param path=Mini.pbip --var Build.Reason=PullRequest --result run.json
"""

import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile

VSO = re.compile(r"##vso\[([\w.]+)([^\]]*)\](.*)")
PARAMETER = re.compile(r"\$\{\{\s*parameters\.(\w+)\s*\}\}")
MACRO = re.compile(r"\$\(([\w.]+)\)")
IF = re.compile(r"\$\{\{\s*if\s+parameters\.(\w+)\s*\}\}")


class TemplateError(Exception):
    pass


def render(node, params):
    """Expands template expressions, as the pipeline compiles the template."""
    if isinstance(node, str):
        whole = PARAMETER.fullmatch(node.strip())
        if whole:
            return params[whole[1]]
        if "${{" in PARAMETER.sub("", node):
            raise TemplateError(f"unsupported template expression in {node!r}")
        return PARAMETER.sub(lambda m: text(params[m[1]]), node)
    if isinstance(node, list):
        items = []
        for item in node:
            if isinstance(item, dict) and len(item) == 1 and next(iter(item)).startswith("${{"):
                key, body = next(iter(item.items()))
                condition = IF.fullmatch(key.strip())
                if not condition:
                    raise TemplateError(f"unsupported template expression {key!r}")
                if params[condition[1]]:
                    items.extend(render(body, params))
            else:
                items.append(render(item, params))
        return items
    if isinstance(node, dict):
        return {render(k, params): render(v, params) for k, v in node.items()}
    return node


def text(value) -> str:
    """A value as the pipeline turns it into a string: booleans as True/False."""
    return str(value)


def expand(value: str, variables: dict[str, str]) -> str:
    return MACRO.sub(lambda m: variables.get(m[1], m[0]), value)


def env_name(variable: str) -> str:
    return variable.replace(".", "_").upper()


def parameters(template: dict, overrides: dict[str, str]) -> dict:
    params = {}
    for spec in template.get("parameters", []):
        value = spec.get("default")
        if spec["name"] in overrides:
            raw = overrides.pop(spec["name"])
            value = raw.lower() == "true" if spec.get("type") == "boolean" else raw
        if "values" in spec and value not in spec["values"]:
            raise TemplateError(f"{spec['name']}: {value!r} is not one of {spec['values']}")
        params[spec["name"]] = value
    if overrides:
        raise TemplateError(f"unknown parameters: {', '.join(overrides)}")
    return params


# Conditions: a tiny recursive-descent evaluator over the forms listed above.
TOKEN = re.compile(r"\s*(?:(\w+)|('(?:[^']|'')*')|(\[|\]|\(|\)|,))")


def tokenize(source: str) -> list[str]:
    tokens, pos = [], 0
    while pos < len(source.rstrip()):
        match = TOKEN.match(source, pos)
        if not match:
            raise TemplateError(f"cannot parse condition {source!r} at {pos}")
        tokens.append(match[1] or match[2] or match[3])
        pos = match.end()
    return tokens


def evaluate(source: str, status: str, variables: dict[str, str]):
    tokens = tokenize(source)

    def take(expected=None):
        token = tokens.pop(0)
        if expected and token != expected:
            raise TemplateError(f"expected {expected!r}, got {token!r} in {source!r}")
        return token

    def value():
        token = take()
        if token.startswith("'"):
            return token[1:-1].replace("''", "'")
        if token == "variables":
            take("[")
            name = value()
            take("]")
            return variables.get(name, "")
        take("(")
        args = []
        while tokens[0] != ")":
            args.append(value())
            if tokens[0] == ",":
                take(",")
        take(")")
        return call(token, args)

    def call(name, args):
        functions = {
            "succeeded": lambda: status == "Succeeded",
            "failed": lambda: status == "Failed",
            "always": lambda: True,
            "succeededOrFailed": lambda: True,
            "and": lambda *a: all(truthy(x) for x in a),
            "or": lambda *a: any(truthy(x) for x in a),
            "not": lambda a: not truthy(a),
            "eq": lambda a, b: str(a).lower() == str(b).lower(),
            "ne": lambda a, b: str(a).lower() != str(b).lower(),
        }
        if name not in functions:
            raise TemplateError(f"unsupported function {name}() in {source!r}")
        return functions[name](*args)

    result = value()
    if tokens:
        raise TemplateError(f"trailing tokens {tokens} in {source!r}")
    return truthy(result)


def truthy(value) -> bool:
    return value if isinstance(value, bool) else value not in ("", "0", "false", "False")


class Agent:
    def __init__(self, variables: dict[str, str], workdir: Path):
        self.variables = variables
        self.workdir = workdir
        self.path: list[str] = []
        self.status = "Succeeded"
        self.steps: list[dict] = []
        self.issues: list[dict] = []
        self.summaries: list[str] = []
        self.artifacts: list[dict] = []

    def run(self, steps: list[dict]):
        for step in steps:
            label = step.get("displayName") or step.get("name") or "step"
            condition = step.get("condition", "succeeded()")
            if not evaluate(condition, self.status, self.variables):
                self.steps.append({"displayName": label, "result": "Skipped"})
                print(f"== {label}: skipped ({condition})", flush=True)
                continue
            print(f"== {label}", flush=True)
            if "bash" in step:
                ok = self.bash(step)
            elif step.get("task") == "PublishBuildArtifacts@1":
                ok = self.publish(step)
            else:
                raise TemplateError(f"unsupported step {sorted(step)}")
            if ok:
                result = "Succeeded"
            elif truthy(step.get("continueOnError", False)):
                result = "SucceededWithIssues"
            else:
                result = "Failed"
                self.status = "Failed"
            self.steps.append({"displayName": label, "result": result})

    def environment(self, step: dict) -> dict[str, str]:
        env = dict(os.environ)
        for name, value in self.variables.items():
            env[env_name(name)] = value
        for name, value in (step.get("env") or {}).items():
            env[name] = expand(text(value), self.variables)
        if self.path:
            env["PATH"] = os.pathsep.join([*reversed(self.path), env.get("PATH", "")])
        return env

    def bash(self, step: dict) -> bool:
        workdir = Path(expand(step.get("workingDirectory", str(self.workdir)), self.variables))
        with tempfile.NamedTemporaryFile("w", suffix=".sh", delete=False, newline="\n") as script:
            script.write(step["bash"])
        env = self.environment(step)
        try:
            process = subprocess.run(
                # Found on PATH: Windows would try System32's WSL bash first.
                [shutil.which("bash") or "bash", "--noprofile", "--norc", script.name],
                cwd=workdir, env=env, stdout=subprocess.PIPE, text=True,
            )
        finally:
            os.unlink(script.name)
        for line in process.stdout.splitlines():
            print(line, flush=True)
            self.command(line.rstrip("\r"), step.get("name"))
        print(f"   exit {process.returncode}", flush=True)
        return process.returncode == 0

    def command(self, line: str, step_name: str | None):
        match = VSO.search(line)
        if not match:
            return
        name, raw, message = match[1], match[2].strip(), match[3]
        props = dict(p.split("=", 1) for p in raw.split(";") if "=" in p)
        if name == "task.setvariable":
            variable = props["variable"]
            if props.get("isoutput", props.get("isOutput", "")).lower() == "true":
                if not step_name:
                    raise TemplateError("an output variable from a step without a name")
                variable = f"{step_name}.{variable}"
            self.variables[variable] = message
        elif name == "task.prependpath":
            self.path.append(message)
        elif name == "task.logissue":
            self.issues.append({**props, "message": message})
        elif name == "task.uploadsummary":
            self.summaries.append(message)
        else:
            raise TemplateError(f"unsupported logging command {name}")

    def publish(self, step: dict) -> bool:
        inputs = {k: expand(text(v), self.variables) for k, v in step["inputs"].items()}
        path = Path(inputs["PathtoPublish"])
        if not path.exists():
            print(f"   no such path: {path}", flush=True)
            return False
        self.artifacts.append({"name": inputs["ArtifactName"], "path": str(path)})
        return True


def pairs(items: list[str], flag: str) -> dict[str, str]:
    result = {}
    for item in items:
        if "=" not in item:
            raise SystemExit(f"{flag} takes NAME=VALUE, not {item!r}")
        name, value = item.split("=", 1)
        result[name] = value
    return result


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("template", type=Path)
    parser.add_argument("--param", action="append", default=[], metavar="NAME=VALUE")
    parser.add_argument("--var", action="append", default=[], metavar="NAME=VALUE",
                        help="a pipeline variable, e.g. Build.Reason=PullRequest")
    parser.add_argument("--result", type=Path, required=True, help="where to write the JSON result")
    args = parser.parse_args(argv)
    sys.stdout.reconfigure(errors="replace")

    import yaml  # only here, so the evaluator's tests run without PyYAML

    workdir = Path.cwd()
    variables = {
        "System.DefaultWorkingDirectory": str(workdir),
        "Build.SourcesDirectory": str(workdir),
        "Agent.TempDirectory": tempfile.mkdtemp(prefix="ado-temp-"),
        "Agent.OS": {"win32": "Windows_NT", "darwin": "Darwin"}.get(sys.platform, "Linux"),
        "Build.Reason": "Manual",
        **pairs(args.var, "--var"),
    }
    template = yaml.safe_load(args.template.read_text(encoding="utf-8"))
    steps = render(template["steps"], parameters(template, pairs(args.param, "--param")))
    agent = Agent(variables, workdir)
    agent.run(steps)
    result = {
        "result": agent.status,
        "steps": agent.steps,
        "variables": agent.variables,
        "issues": agent.issues,
        "summaries": agent.summaries,
        "artifacts": agent.artifacts,
    }
    args.result.write_text(json.dumps(result, indent=2), encoding="utf-8")
    print(f"== job {agent.status}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
