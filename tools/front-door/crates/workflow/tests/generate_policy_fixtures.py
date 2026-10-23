"""Translate pure Python policy assertions into retained JSON fixtures.

Run from this directory with the repository root as argv[1]. No pytest needed.
Tests run with their original assertions; only pytest's parametrisation data is
expanded here. Filesystem/registry drift tests are outside this pure boundary.
"""
import ast
import itertools
import json
from pathlib import Path
import sys
from types import SimpleNamespace

root = Path(sys.argv[1]).resolve()
sys.path.insert(0, str(root))

def parametrise(names, cases):
    def decorate(function):
        specs = getattr(function, "parameters", [])
        function.parameters = specs + [(names.split(",") if isinstance(names, str) else names, cases)]
        return function
    return decorate

pytest = SimpleNamespace(mark=SimpleNamespace(parametrize=parametrise), param=lambda *values, **kw: values)
for unit, function_name in [("review_ladder", "check_review_ladder"), ("review_terminal", "normalise_dispatch_review")]:
    source = root / f"tests/test_{unit}.py"
    tree = ast.parse(source.read_text())
    tree.body = [node for node in tree.body if not (isinstance(node, ast.Import) and any(n.name == "pytest" for n in node.names))]
    namespace = {"pytest": pytest, "__file__": str(source)}
    exec(compile(tree, str(source), "exec"), namespace)
    original = namespace[function_name]
    fixtures = []
    def record(*args, **kwargs):
        expected = original(*args, **kwargs)
        if unit == "review_ladder":
            inputs = {"risk":args[0], "legs":args[1], "chair":kwargs.get("chair_family")}
        else:
            inputs = {"dispatch":args[0], "terminal":args[1], "verdict":kwargs["review_verdict"], "chair":kwargs["chair_family"], "transcript":kwargs["transcript_available"], "output":kwargs["dispatcher_output_available"]}
        fixtures.append({"input":inputs, "expected":expected})
        return expected
    namespace[function_name] = record
    for name, function in list(namespace.items()):
        if not name.startswith("test_") or name == "test_every_implemented_adapter_has_an_endpoint_provider":
            continue
        specs = getattr(function, "parameters", [])
        for combination in itertools.product(*(cases for names, cases in specs)):
            kwargs = {}
            for (names, cases), values in zip(specs, combination):
                if len(names) == 1:
                    # pytest.param with one argument is a one-tuple, unlike a raw case.
                    if isinstance(values, tuple) and len(values) == 1:
                        values = values[0]
                    values = (values,)
                kwargs.update(zip(names, values))
            function(**kwargs)
    destination = Path(__file__).parent / "fixtures" / f"{unit}.json"
    destination.parent.mkdir(exist_ok=True)
    destination.write_text("[\n" + ",\n".join(json.dumps(case, sort_keys=True) for case in fixtures) + "\n]\n")
    print(f"{unit}: {len(fixtures)} translated cases")
