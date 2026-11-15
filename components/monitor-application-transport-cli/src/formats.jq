# The formats of jq that jaq's standard library leaves out: a row (an
# array of strings, numbers, booleans and nulls) as a line of a table.

def @csv: map(
    if type == "string" then "\"" + (split("\"") | join("\"\"")) + "\""
    elif type == "null" then ""
    elif type == "number" or type == "boolean" then tostring
    else error("\(type) (\(tojson)) is not valid in a csv row")
    end
  ) | join(",");

def @tsv: map(
    if type == "string" then
      split("\\") | join("\\\\")
      | split("\t") | join("\\t")
      | split("\n") | join("\\n")
      | split("\r") | join("\\r")
    elif type == "null" then ""
    elif type == "number" or type == "boolean" then tostring
    else error("\(type) (\(tojson)) is not valid in a tsv row")
    end
  ) | join("\t");
