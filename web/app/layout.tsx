import { Geist_Mono, Manrope, Outfit } from "next/font/google"

import "./globals.css"
import { ThemeProvider } from "@/components/theme-provider"
import { cn } from "@/lib/utils";
import { TooltipProvider } from "@/components/ui/tooltip";

const outfitHeading = Outfit({subsets:['latin'],variable:'--font-heading'});

const manrope = Manrope({subsets:['latin'],variable:'--font-sans'})

const fontMono = Geist_Mono({
  subsets: ["latin"],
  variable: "--font-mono",
})

export default function RootLayout({
  children,
}: Readonly<{
  children: React.ReactNode
}>) {
  return (
    <html
      lang="en"
      suppressHydrationWarning
      className={cn("antialiased", fontMono.variable, "font-sans", manrope.variable, outfitHeading.variable)}
    >
      <body>
        <ThemeProvider><TooltipProvider>{children}</TooltipProvider></ThemeProvider>
      </body>
    </html>
  )
}
