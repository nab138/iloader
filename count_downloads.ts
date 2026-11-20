// Add up all the download counts for all releases of the project, excluding latest.json and .sig files.
// This is used to track the total number of downloads across all releases, since GitHub doesn't show it for some reason.

const REPO = "nab138/iloader";
const API_URL = `https://api.github.com/repos/${REPO}/releases?per_page=100`;
const TOKEN = process.env.GITHUB_TOKEN;

async function fetchReleases(url: string): Promise<[any[], string | null]> {
  const headers: Record<string, string> = { "User-Agent": "download-counter" };
  if (TOKEN) headers["Authorization"] = `Bearer ${TOKEN}`;

  const resp = await fetch(url, { headers });
  const data = await resp.json();

  if (!resp.ok) {
    throw new Error(`GitHub API error ${resp.status}: ${JSON.stringify(data)}`);
  }
  if (!Array.isArray(data)) {
    throw new Error(`Unexpected response: ${JSON.stringify(data)}`);
  }

  const linkHeader = resp.headers.get("Link") ?? "";
  let nextUrl: string | null = null;
  for (const part of linkHeader.split(",")) {
    if (part.includes('rel="next"')) {
      nextUrl = part.split(";")[0].trim().replace(/[<>]/g, "");
    }
  }
  return [data, nextUrl];
}

async function main() {
  const allReleases: any[] = [];
  let url: string | null = API_URL;

  while (url) {
    const [releases, nextUrl] = await fetchReleases(url);
    allReleases.push(...releases);
    url = nextUrl;
  }

  let total = 0;
  for (const release of allReleases) {
    for (const asset of release.assets ?? []) {
      if (asset.name === "latest.json" || asset.name.endsWith(".sig")) continue;
      total += asset.download_count;
    }
  }

  console.log(total);
}

main();
