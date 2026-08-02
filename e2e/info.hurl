GET {{baseurl}}/info
HTTP 200
[Asserts]
jsonpath "$.std.name" == "servo"
jsonpath "$.std.version" isString
jsonpath "$.std.capabilities['wasi:sockets']" exists
jsonpath "$.std.capabilities['wasi:filesystem']" exists
