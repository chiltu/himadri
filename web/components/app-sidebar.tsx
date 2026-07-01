"use client"

import * as React from "react"

import { NavMain } from "@/components/nav-main"
import { NavProjects } from "@/components/nav-projects"
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
import { LayoutBottomIcon, AudioWave01Icon, CommandIcon, ComputerTerminalIcon, RoboticIcon, BookOpen02Icon, Settings05Icon, CropIcon, PieChartIcon, MapsIcon } from "@hugeicons/core-free-icons"

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
          title: "All Models",
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
        <HugeiconsIcon icon={BookOpen02Icon} strokeWidth={2} />
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
  projects: [
    {
      name: "Design Engineering",
      url: "#",
      icon: (
        <HugeiconsIcon icon={CropIcon} strokeWidth={2} />
      ),
    },
    {
      name: "Sales & Marketing",
      url: "#",
      icon: (
        <HugeiconsIcon icon={PieChartIcon} strokeWidth={2} />
      ),
    },
    {
      name: "Travel",
      url: "#",
      icon: (
        <HugeiconsIcon icon={MapsIcon} strokeWidth={2} />
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
        <NavProjects projects={data.projects} />
      </SidebarContent>
      <SidebarFooter>
        <NavUser user={data.user} />
      </SidebarFooter>
      <SidebarRail />
    </Sidebar>
  )
}
