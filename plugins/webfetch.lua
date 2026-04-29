local function trim(value)
    return (value:gsub("^%s+", ""):gsub("%s+$", ""))
end

local function html_unescape(value)
    return value
        :gsub("&nbsp;", " ")
        :gsub("&amp;", "&")
        :gsub("&lt;", "<")
        :gsub("&gt;", ">")
        :gsub("&quot;", '"')
        :gsub("&#39;", "'")
end

local function html_to_text(value)
    value = value:gsub("\r\n", "\n"):gsub("\r", "\n")
    value = value:gsub("<!%-%-.-%-%->", "\n")
    value = value:gsub("<[Ss][Cc][Rr][Ii][Pp][Tt][^>]*>.-</[Ss][Cc][Rr][Ii][Pp][Tt]>", "\n")
    value = value:gsub("<[Ss][Tt][Yy][Ll][Ee][^>]*>.-</[Ss][Tt][Yy][Ll][Ee]>", "\n")
    value = value:gsub("<[Bb][Rr]%s*/?>", "\n")
    value = value:gsub("</[Pp]>", "\n")
    value = value:gsub("</[Dd][Ii][Vv]>", "\n")
    value = value:gsub("</[Ll][Ii]>", "\n")
    value = value:gsub("<[^>]+>", " ")
    value = html_unescape(value)
    value = value:gsub("[ \t]+", " ")
    value = value:gsub(" *\n *", "\n")
    value = value:gsub("\n\n\n+", "\n\n")
    return trim(value)
end

local function truncate(value, max_chars)
    if #value <= max_chars then
        return value
    end
    return value:sub(1, max_chars) .. "\n\n[truncated, " .. #value .. " total characters]"
end

tool {
    name = "webfetch",
    description = "Fetch content from a web URL so the assistant can read it. Returns cleaned text by default, or raw response content when raw=true.",
    parameters = {
        type = "object",
        properties = {
            url = {
                type = "string",
                description = "Fully qualified http:// or https:// URL to fetch"
            },
            raw = {
                type = "boolean",
                description = "Return the raw response body instead of cleaned text (default: false)"
            },
            max_chars = {
                type = "integer",
                description = "Maximum characters to return, from 1000 to 50000 (default: 20000)"
            }
        },
        required = { "url" }
    },
    handler = function(args)
        local url = args.url or ""
        if url == "" then
            return "Error: url is required"
        end
        if not (url:match("^https?://")) then
            return "Error: url must start with http:// or https://"
        end
        if url:match("%s") then
            return "Error: url must not contain whitespace"
        end

        local max_chars = tonumber(args.max_chars) or 20000
        if max_chars < 1000 then
            max_chars = 1000
        elseif max_chars > 50000 then
            max_chars = 50000
        end

        local ok, body = pcall(http_get, url)
        if not ok or not body then
            return "Error: failed to fetch URL"
        end

        local content = args.raw and body or html_to_text(body)
        if content == "" then
            content = body
        end

        return "URL: " .. url .. "\nCharacters: " .. #content .. "\n\n" .. truncate(content, max_chars)
    end
}
