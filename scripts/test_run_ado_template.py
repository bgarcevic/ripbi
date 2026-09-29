"""The Azure Pipelines emulator's expressions, without running any step."""

import unittest

from run_ado_template import TemplateError, evaluate, expand, parameters, render


class RenderTests(unittest.TestCase):
    def test_parameters_splice_into_strings_and_keep_their_type_alone(self):
        params = {"name": "ripbi", "logIssues": True}
        self.assertEqual(render("$(${{ parameters.name }}.verdict)", params), "$(ripbi.verdict)")
        self.assertIs(render("${{ parameters.logIssues }}", params), True)

    def test_an_if_item_inserts_its_steps_only_when_true(self):
        steps = ["a", {"${{ if parameters.publish }}": ["b", "c"]}, "d"]
        self.assertEqual(render(steps, {"publish": True}), ["a", "b", "c", "d"])
        self.assertEqual(render(steps, {"publish": False}), ["a", "d"])

    def test_unknown_expressions_are_errors(self):
        with self.assertRaises(TemplateError):
            render("${{ variables.x }}", {})
        with self.assertRaises(TemplateError):
            render([{"${{ each x in parameters.list }}": []}], {})

    def test_parameters_take_defaults_overrides_and_allowed_values(self):
        template = {"parameters": [
            {"name": "path", "type": "string", "default": ""},
            {"name": "logIssues", "type": "boolean", "default": True},
            {"name": "failOn", "type": "string", "default": "auto", "values": ["auto", "new"]},
        ]}
        self.assertEqual(
            parameters(template, {"logIssues": "false", "path": "Mini.pbip"}),
            {"path": "Mini.pbip", "logIssues": False, "failOn": "auto"},
        )
        with self.assertRaises(TemplateError):
            parameters(template, {"failOn": "sometimes"})
        with self.assertRaises(TemplateError):
            parameters(template, {"typo": "x"})


class ConditionTests(unittest.TestCase):
    def test_status_functions(self):
        self.assertTrue(evaluate("succeeded()", "Succeeded", {}))
        self.assertFalse(evaluate("succeeded()", "Failed", {}))
        self.assertTrue(evaluate("always()", "Failed", {}))
        self.assertTrue(evaluate("failed()", "Failed", {}))

    def test_variables_and_comparisons(self):
        variables = {"ripbi.sarifFile": "/tmp/ripbi.sarif"}
        condition = "and(succeeded(), ne(variables['ripbi.sarifFile'], ''))"
        self.assertTrue(evaluate(condition, "Succeeded", variables))
        self.assertFalse(evaluate(condition, "Succeeded", {}))
        self.assertTrue(evaluate("eq('A', 'a')", "Succeeded", {}))
        self.assertTrue(evaluate("not(eq('it''s', 'its'))", "Succeeded", {}))

    def test_unknown_functions_are_errors(self):
        with self.assertRaises(TemplateError):
            evaluate("contains('ab', 'a')", "Succeeded", {})


class MacroTests(unittest.TestCase):
    def test_unknown_macros_stay_as_written(self):
        variables = {"ripbi.verdict": "pass"}
        self.assertEqual(expand("$(ripbi.verdict) $(System.AccessToken)", variables),
                         "pass $(System.AccessToken)")


if __name__ == "__main__":
    unittest.main()
