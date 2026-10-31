import { blankConnection } from "./cloud";

// Only provider-owned S3 hostnames are inferred. SDKs receive a service endpoint,
// never a bucket host. The editor pairs these with path-style SDK requests.
export function parseS3Url(value) {
  const unknown = { endpoint: value.trim(), bucket: "", region: "", known: false };
  let url;
  try { url = new URL(value); } catch { return unknown; }
  if (!["https:", "http:"].includes(url.protocol) || url.username || url.password || url.search || url.hash || url.port) return unknown;
  const host = url.hostname;
  let match = host.match(/^(?:([a-z0-9][a-z0-9.-]*)\.)?s3\.([a-z0-9-]+)\.qiniucs\.com$/);
  let service;
  let bucket = "";
  let region = "";
  if (match) { bucket = match[1] || ""; region = match[2]; service = `s3.${region}.qiniucs.com`; }
  else {
    match = host.match(/^(?:([a-z0-9][a-z0-9.-]*)\.)?s3[.-]((?:us|eu|ap|sa|ca|cn|me|af|il|mx)-(?:[a-z]+-)*[0-9]+)\.amazonaws\.com(\.cn)?$/);
    if (match) { bucket = match[1] || ""; region = match[2]; service = `s3.${region}.amazonaws.com${match[3] || ""}`; }
    else {
      match = host.match(/^(?:([a-z0-9][a-z0-9.-]*)\.)?s3\.amazonaws\.com$/);
      if (match) { bucket = match[1] || ""; region = "us-east-1"; service = "s3.amazonaws.com"; }
    }
  }
  if (!service) return unknown;
  const parts = url.pathname.split("/").filter(Boolean);
  if (bucket && parts.length || parts.length > 1 || parts.some((part) => !/^[a-z0-9][a-z0-9.-]*$/.test(part))) return unknown;
  if (!bucket && parts.length === 1) bucket = parts[0];
  return { endpoint: `${url.protocol}//${service}`, bucket, region, known: true };
}

export function connectionEditor(item) {
  return { ...blankConnection(), ...item, accessKey: "", secretKey: "", sessionToken: "", proxy: { mode: "direct", ...item?.proxy, username: "", password: "" } };
}
