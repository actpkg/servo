POST {{baseurl}}/tools
HTTP 200
[Asserts]
jsonpath "$.tools[*].name" contains "render"
jsonpath "$.tools[*].name" contains "dom"
jsonpath "$.tools[*].name" contains "eval"
jsonpath "$.tools[*].name" contains "click"
jsonpath "$.tools[*].name" contains "scroll"
jsonpath "$.tools[*].name" contains "screenshot"
# Deliberately absent: an instance renders one document, so there is nothing to
# navigate to. See skill/SKILL.md.
jsonpath "$.tools[*].name" not contains "navigate"
