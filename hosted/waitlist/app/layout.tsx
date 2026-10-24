import type { Metadata } from "next";
import "./globals.css";

export const metadata: Metadata = {
  title: "Join the hosted storage waitlist · openxet",
  description: "Request access to hosted openxet. Your email is saved privately and used only to contact you about launch access.",
  robots: { index: false, follow: false },
  referrer: "strict-origin",
  icons: {
    icon: "/favicon.svg",
    shortcut: "/favicon.svg",
  },
};

export default function RootLayout({
  children,
}: Readonly<{
  children: React.ReactNode;
}>) {
  return (
    <html lang="en">
      <body>{children}</body>
    </html>
  );
}
