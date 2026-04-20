tool {
    name = "weather",
    description = "Get current weather conditions and a 3-day forecast for any location. Supports city names, zip codes, airport codes, and coordinates.",
    parameters = {
        type = "object",
        properties = {
            location = {
                type = "string",
                description = "City name, zip/postal code, airport IATA code, or 'lat,lon' (e.g. 'San Francisco', 'SW1A', 'JFK', '40.71,-74.01')"
            },
            units = {
                type = "string",
                description = "Temperature units: 'c' for Celsius (default) or 'f' for Fahrenheit"
            }
        },
        required = { "location" }
    },
    handler = function(args)
        local location = args.location or ""
        if location == "" then
            return "Error: no location provided"
        end
        if location:match("[;&|`]") then
            return "Error: invalid characters in location"
        end

        local use_f = (args.units or "c"):lower() == "f"
        location = location:gsub("'", ""):gsub(" ", "+")

        local ok, body = pcall(http_get, "https://wttr.in/" .. location .. "?format=j1")
        if not ok or not body then
            return "Error: failed to fetch weather data"
        end

        local ok2, data = pcall(json_decode, body)
        if not ok2 or not data then
            return "Error: failed to parse weather data"
        end

        local cc = data.current_condition and data.current_condition[1]
        if not cc then
            return "Error: no weather data in response"
        end

        local na = data.nearest_area and data.nearest_area[1]
        local u = use_f and "\194\176F" or "\194\176C"
        local wu = use_f and "mph" or "km/h"
        local tk = use_f and "temp_F" or "temp_C"
        local fk = use_f and "FeelsLikeF" or "FeelsLikeC"
        local wk = use_f and "windspeedMiles" or "windspeedKmph"
        local maxk = use_f and "maxtempF" or "maxtempC"
        local mink = use_f and "mintempF" or "mintempC"

        local area = "Unknown location"
        if na then
            local an = na.areaName and na.areaName[1] and na.areaName[1].value
            local rg = na.region and na.region[1] and na.region[1].value
            if an and rg then
                area = an .. ", " .. rg
            end
        end

        local temp = cc[tk] or "?"
        local feels = cc[fk] or "?"
        local desc = cc.weatherDesc and cc.weatherDesc[1] and cc.weatherDesc[1].value or "?"
        desc = desc:gsub("^%s+", ""):gsub("%s+$", "")
        local humidity = cc.humidity or "?"
        local wind = cc[wk] or "?"
        local wind_dir = cc.winddir16Point or "?"
        local precip = cc.precipMM or "0"
        local vis = cc.visibility or "?"
        local uv = cc.uvIndex or "?"

        local forecast_lines = {}
        if data.weather then
            for _, day in ipairs(data.weather) do
                if day.date and day[maxk] and day[mink] then
                    forecast_lines[#forecast_lines + 1] = string.format(
                        "  %s  High %s%s  Low %s%s",
                        day.date, day[maxk], u, day[mink], u
                    )
                end
            end
        end

        local out = {
            area,
            desc .. ", " .. temp .. u .. " (feels like " .. feels .. u .. ")",
            "Humidity: " .. humidity .. "%",
            "Wind: " .. wind .. " " .. wu .. " " .. wind_dir,
            "Visibility: " .. vis .. " km",
            "Precipitation: " .. precip .. " mm",
            "UV Index: " .. uv,
            "",
            "3-Day Forecast:",
            table.concat(forecast_lines, "\n"),
        }

        return table.concat(out, "\n")
    end
}
