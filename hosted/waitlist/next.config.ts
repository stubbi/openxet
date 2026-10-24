import type { NextConfig } from "next";

const nextConfig: NextConfig = {
  async headers() {
    return [{
      source: "/:path*",
      headers: [
        { key: "Cache-Control", value: "no-store" },
        { key: "X-Robots-Tag", value: "noindex, nofollow" },
        { key: "Referrer-Policy", value: "strict-origin" },
        { key: "X-Content-Type-Options", value: "nosniff" },
        { key: "X-Frame-Options", value: "DENY" },
      ],
    }];
  },
};

export default nextConfig;
