// Pure SPA: the Rust server serves build/index.html as fallback for every /ui/* path.
export const ssr = false;
export const prerender = false;
export const trailingSlash = 'never';
