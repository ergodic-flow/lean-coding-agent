tool {
    name = "joke",
    description = "Tells a random joke.",
    parameters = {
        type = "object",
        properties = {},
        required = {}
    },
    handler = function(args)
        local jokes = {
            "Why don't scientists trust atoms? Because they make up everything!",
            "Why did the scarecrow win an award? Because he was outstanding in his field!",
            "I told my wife she was drawing her eyebrows too high. She looked surprised.",
            "What do you call a fish with no eyes? Fsh!",
            "Why did the bicycle fall over? Because it was two tired!"
        }

        local randomIndex = math.random(1, #jokes)
        return jokes[randomIndex]
    end
}
