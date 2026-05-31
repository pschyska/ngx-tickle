done = function(summary, latency, requests)
  local out = os.getenv("WRK_REPORT")
  if not out then
    error("WRK_REPORT must be set")
  end
  io.output(out)

  local extra_fields = os.getenv("WRK_EXTRA_FIELDS") or ""

  local extra = {}
  local extra_keys = {}
  for k, v in extra_fields:gmatch("([^=,]+)=([^,]*)") do
    extra[k] = v
    extra_keys[#extra_keys + 1] = k
  end

  local ps = {50, 90, 99, 99.9}

  for _, k in ipairs(extra_keys) do
    io.write(string.format("%s,", k))
  end
  io.write("path,rps,min,max,mean,stdev")
  for _, p in pairs(ps) do
    io.write(string.format(",%g%%", p))
  end
  io.write("\n")

  local r = summary["requests"]
  local s = summary["duration"] / 1e6
  local rps = r / s
  for _, k in ipairs(extra_keys) do
    io.write(string.format("%s,", extra[k]))
  end
  io.write(string.format("%s", wrk.path))
  io.write(string.format(",%d", rps))
  io.write(string.format(",%d", latency.min))
  io.write(string.format(",%d", latency.max))
  io.write(string.format(",%d", latency.mean))
  io.write(string.format(",%d", latency.stdev))
  for _, p in pairs(ps) do
    n = latency:percentile(p)
    io.write(string.format(",%d", n))
  end
  io.write("\n")
end
