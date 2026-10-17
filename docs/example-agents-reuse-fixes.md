# Example: agents reuse each other's fixes

An agent that solves a problem records the fix as a memd case. When another
agent hits the same error, in another project or on another machine, it finds
the case and applies the fix.

<figure class="memd-figure">
--8<-- "docs/figures/fix-reuse.svg"
</figure>

## The test

Each session is a new agent with one task: run a small command-line tool and
report what it prints. The tool stops with an error that the project does not
explain:

```console
$ bin/labkit summarize samples.tsv
labkit: error E-LK17: calibration lattice unavailable (state=sealed)
```

The fix needs a value that only one other project's notes contain. The tool
checks the value against a hash, so its code does not give the value away. The
task does not mention memd, but the agents' global instructions tell them to
search the recorded cases after an unexpected error and to record a case after
they fix a problem.

The agents wrote each case in memd's case format and sent it to a small service
that all test machines could reach.

## Results

<figure class="memd-figure">
--8<-- "docs/figures/fix-reuse-results.svg"
<figcaption>Each row is one session. The line runs from the tool's first error to the fix, or to the point where the agent gave up.</figcaption>
</figure>

All 5 sessions that had a memd case solved the problem. Four found the case
through the service. The one with the service blocked found it in its machine's
memd store. Of the 4 sessions without a case, 1 solved the problem, by reading
another project's notes. The tools are small test fixtures, and the counts
include each agent's own bookkeeping, so read the chart as a demonstration, not
a benchmark.

In a third test, the case said that the value differs from project to project.
The agent that found the case did not reuse the other project's value. It
asked for its own project's value instead.

## Set it up

1. Install memd 1.9.0 or later with its agent skill and rules:

    ```bash
    git clone --depth 1 https://github.com/fmschulz/memd && cd memd
    make install
    ```

    The rules tell Claude Code and Codex to search the cases after an
    unexpected failure and to record a case after they solve a problem.

2. In its workspace-write sandbox, Codex can write only to the project and to
    temporary directories. Searches still work there, because memd opens the
    store read-only. To let Codex record cases, make the memd directory
    writable:

    ```toml
    # ~/.codex/config.toml
    [sandbox_workspace_write]
    writable_roots = ["/home/you/.memd"]  # use your home directory
    ```

3. In each project, run `memd init` with the same tenant, so that the projects
    share their cases:

    ```bash
    memd init --tenant-id lab --project-id my-project
    ```

4. From then on, the agents record and search cases themselves. When an agent
    fixes a problem, it writes a short case file and records it with
    `memd experience note case.json`. A case for test 1 looks like this:

    ```json
    {
      "problem": "labkit stops with E-LK17 before calibration is set up",
      "symptoms": ["labkit", "E-LK17", "calibration lattice unavailable"],
      "failed": ["run summarize again"],
      "fix": "set LABKIT_HOME, then bin/labkit init --profile ncl-lattice-r7",
      "check": {"method": "observed", "result": "summarize exited 0"},
      "applies_when": {"tool": "labkit"}
    }
    ```

    An agent in another project that hits the same error finds the case:

    ```bash
    memd experience find "E-LK17 calibration lattice" --tool labkit
    ```

5. To give another machine the case, export it and import the file there:

    ```bash
    # on the machine with the case
    memd experience export PROBLEM_ID > cases.json
    # on the other machine
    memd experience import cases.json --project-id my-project
    ```

    `PROBLEM_ID` is the `problem_id` that `note` printed. `--project-id`
    names the project that recorded the case.

[Experience memory](experience-memory.md) describes the case fields and how
`find` decides whether a case applies.
