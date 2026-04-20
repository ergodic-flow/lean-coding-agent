tool {
    name = "arithmetic",
    description = "Perform basic arithmetic operations (add, subtract, multiply, divide) on two numbers.",
    parameters = {
        type = "object",
        properties = {
            operation = {
                type = "string",
                description = "The operation: add, subtract, multiply, or divide"
            },
            a = {
                type = "number",
                description = "First operand"
            },
            b = {
                type = "number",
                description = "Second operand"
            }
        },
        required = { "operation", "a", "b" }
    },
    handler = function(args)
        local op = args.operation
        local a = tonumber(args.a)
        local b = tonumber(args.b)

        if not op then
            return "Error: 'operation' is required"
        end
        if not a or not b then
            return "Error: 'a' and 'b' must be numbers"
        end

        if op == "add" then
            return string.format("%.10g", a + b)
        elseif op == "subtract" then
            return string.format("%.10g", a - b)
        elseif op == "multiply" then
            return string.format("%.10g", a * b)
        elseif op == "divide" then
            if b == 0 then
                return "Error: division by zero"
            end
            return string.format("%.10g", a / b)
        else
            return "Error: unknown operation '" .. op .. "'. Use add, subtract, multiply, or divide."
        end
    end
}
