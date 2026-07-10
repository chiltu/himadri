"use client"

import * as React from "react"

import { NavMain } from "@/components/nav-main"
import { NavUser } from "@/components/nav-user"
import { TeamSwitcher } from "@/components/team-switcher"
import {
  Sidebar,
  SidebarContent,
  SidebarFooter,
  SidebarHeader,
  SidebarRail,
} from "@/components/ui/sidebar"
import { HugeiconsIcon } from "@hugeicons/react"
import { LayoutBottomIcon, AudioWave01Icon, CommandIcon, ComputerTerminalIcon, RoboticIcon, Settings05Icon, DashboardSquareIcon, DollarIcon, CropIcon, PieChartIcon, MapsIcon, LockKeyIcon, User, UserGroupIcon, HelpSquareIcon } from "@hugeicons/core-free-icons"
import { Separator } from "./ui/separator"
import { NavSettings } from "./nav-settings"

// This is sample data.
const data = {
  user: {
    name: "shadcn",
    email: "m@example.com",
    avatar: "/avatars/shadcn.jpg",
  },
  teams: [
    {
      name: "Acme Inc",
      logo: (
        <HugeiconsIcon icon={LayoutBottomIcon} strokeWidth={2} />
      ),
      plan: "Enterprise",
    },
    {
      name: "Acme Corp.",
      logo: (
        <HugeiconsIcon icon={AudioWave01Icon} strokeWidth={2} />
      ),
      plan: "Startup",
    },
    {
      name: "Evil Corp.",
      logo: (
        <HugeiconsIcon icon={CommandIcon} strokeWidth={2} />
      ),
      plan: "Free",
    },
  ],
  navMain: [
    {
      title: "Home",
      url: "/dashboard",
      icon: (
        <HugeiconsIcon icon={DashboardSquareIcon} strokeWidth={2} />
      ),
      isActive: true,
      items: [
        {
          title: "Dashboard",
          url: "/dashboard"
        }
      ]
    },
    {
      title: "Playground",
      url: "#",
      icon: (
        <HugeiconsIcon icon={ComputerTerminalIcon} strokeWidth={2} />
      ),
      isActive: true,
      items: [
        {
          title: "Try out",
          url: "/dashboard/playground",
        },
      ],
    },
    {
      title: "Models",
      url: "#",
      icon: (
        <HugeiconsIcon icon={RoboticIcon} strokeWidth={2} />
      ),
      items: [
        {
          title: "Models",
          url: "/dashboard/models",
        },
        {
          title: "Logs",
          url: "/dashboard/logs",
        },
      ],
    },
    {
      title: "Cost",
      url: "#",
      icon: (
        <HugeiconsIcon icon={DollarIcon} strokeWidth={2} />
      ),
      items: [
        {
          title: "Usage",
          url: "/dashboard/usage",
        },
      ],
    },
    {
      title: "Settings",
      url: "#",
      icon: (
        <HugeiconsIcon icon={Settings05Icon} strokeWidth={2} />
      ),
      items: [
        {
          title: "Config",
          url: "/dashboard/config",
        },
        {
          title: "Keys",
          url: "/dashboard/keys",
        },
      ],
    },
  ],
  navSettings: [
    {
      name: "API Keys",
      url: "#",
      icon: (
        <HugeiconsIcon icon={LockKeyIcon} strokeWidth={2} />
      ),
    },
    {
      name: "Routing Targets",
      url: "#",
      icon: (
        <HugeiconsIcon icon={PieChartIcon} strokeWidth={2} />
      ),
    },
    {
      name: "User Management",
      url: "#",
      icon: (
        <HugeiconsIcon icon={UserGroupIcon} strokeWidth={2} />
      ),
    },
    {
      name: "Documentation",
      url: "#",
      icon: (
        <HugeiconsIcon icon={HelpSquareIcon} strokeWidth={2} />
      ),
    },
  ],
}

export function AppSidebar({ ...props }: React.ComponentProps<typeof Sidebar>) {
  return (
    <Sidebar collapsible="icon" {...props}>
      <SidebarHeader>
        <TeamSwitcher teams={data.teams} />
      </SidebarHeader>
      <SidebarContent>
        <NavMain items={data.navMain} />
        <NavSettings projects={data.navSettings} />
      </SidebarContent>
      <SidebarFooter>
        <NavUser user={data.user} />
      </SidebarFooter>
      <SidebarRail />
    </Sidebar>
  )
}
