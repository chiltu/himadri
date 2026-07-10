"use client"

import { useEffect, useMemo, useState, useRef } from "react"
import { api, type Model, type ModelEndpoint, getSessionToken } from "@/lib/api"
import { AuthGuard } from "@/components/auth-guard"
import { AppSidebar } from "@/components/app-sidebar"
import {
  Breadcrumb,
  BreadcrumbItem,
  BreadcrumbLink,
  BreadcrumbList,
  BreadcrumbPage,
  BreadcrumbSeparator,
} from "@/components/ui/breadcrumb"
import { Separator } from "@/components/ui/separator"
import {
  SidebarInset,
  SidebarProvider,
  SidebarTrigger,
} from "@/components/ui/sidebar"
import { Button } from "@/components/ui/button"
import { Badge } from "@/components/ui/badge"
import { Textarea } from "@/components/ui/textarea"
import { ScrollArea } from "@/components/ui/scroll-area"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import { HugeiconsIcon } from "@hugeicons/react"
import { SentIcon, DashboardSpeed01Icon, BubbleChatIcon } from "@hugeicons/core-free-icons"

interface Message {
  role: "user" | "assistant" | "system"
  content: string
}

// Model context windows are not exposed by the admin API, so we show usage
// against a conservative default window and estimate tokens from characters
// (~4 chars/token) — labelled as an approximation in the UI.
const CONTEXT_WINDOW = 128_000

function formatTokens(n: number): string {
  return n >= 1000 ? `${Math.round(n / 1000)}K` : `${n}`
}

export default function PlaygroundPage() {
  const [models, setModels] = useState<Model[]>([])
  const [endpoints, setEndpoints] = useState<ModelEndpoint[]>([])
  const [selectedModel, setSelectedModel] = useState("")
  const [messages, setMessages] = useState<Message[]>([])
  const [input, setInput] = useState("")
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const messagesEndRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    api.listModels().then(setModels).catch((e) => setError(e.message))
    api.listAllEndpoints().then(setEndpoints).catch((e) => setError(e.message))
  }, [])

  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: "smooth" })
  }, [messages])

  // A model is usable only when it's enabled *and* has an enabled provider
  // endpoint — otherwise a request would 404 with "no provider configured".
  const activeModelIds = new Set(endpoints.filter((e) => e.enabled).map((e) => e.model_id))
  const enabledModels = models.filter((m) => m.enabled && activeModelIds.has(m.id))

  const usedTokens = useMemo(() => {
    const chars = messages.reduce((sum, m) => sum + m.content.length, 0)
    return Math.ceil(chars / 4)
  }, [messages])

  const sendMessage = async () => {
    if (!input.trim() || !selectedModel || loading) return

    const userMessage: Message = { role: "user", content: input.trim() }
    setMessages((prev) => [...prev, userMessage])
    setInput("")
    setLoading(true)
    setError(null)

    try {
      const response = await fetch(
        `${process.env.NEXT_PUBLIC_API_URL || "http://localhost:8080"}/v1/chat/completions`,
        {
          method: "POST",
          headers: {
            "Content-Type": "application/json",
            // API keys are hashed at rest and never returned after creation,
            // so the playground authenticates with the admin session token.
            // Read via the shared helper — the token lives in sessionStorage,
            // not localStorage.
            Authorization: `Bearer ${getSessionToken() ?? ""}`,
          },
          body: JSON.stringify({
            model: selectedModel,
            messages: [...messages, userMessage].map((m) => ({
              role: m.role,
              content: m.content,
            })),
          }),
        }
      )

      if (!response.ok) {
        const err = await response.json().catch(() => ({ error: { message: "Request failed" } }))
        throw new Error(err.error?.message || `HTTP ${response.status}`)
      }

      const data = await response.json()
      const assistantMessage: Message = {
        role: "assistant",
        content: data.choices?.[0]?.message?.content || "No response",
      }
      setMessages((prev) => [...prev, assistantMessage])
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : "Failed to get response")
    } finally {
      setLoading(false)
    }
  }

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault()
      sendMessage()
    }
  }

  const canSend = Boolean(input.trim()) && Boolean(selectedModel) && !loading

  return (
    <AuthGuard>
      <SidebarProvider>
        <AppSidebar />
        <SidebarInset>
          <header className="flex h-16 shrink-0 items-center gap-2 transition-[width,height] ease-linear group-has-data-[collapsible=icon]/sidebar-wrapper:h-12">
            <div className="flex items-center gap-2 px-4">
              <SidebarTrigger className="-ml-1" />
              <Separator orientation="vertical" className="mr-2 data-vertical:h-4 data-vertical:self-auto" />
              <Breadcrumb>
                <BreadcrumbList>
                  <BreadcrumbItem className="hidden md:block">
                    <BreadcrumbLink href="#">Platform</BreadcrumbLink>
                  </BreadcrumbItem>
                  <BreadcrumbSeparator className="hidden md:block" />
                  <BreadcrumbItem>
                    <BreadcrumbPage>Playground</BreadcrumbPage>
                  </BreadcrumbItem>
                </BreadcrumbList>
              </Breadcrumb>
            </div>
          </header>

          <div className="relative flex h-[calc(100svh-4rem)] flex-col overflow-hidden">
            {error && (
              <div className="mx-4 mt-3 shrink-0 rounded-md bg-destructive/10 p-3 text-sm text-destructive">
                {error}
                <Button variant="ghost" size="sm" className="ml-2" onClick={() => setError(null)}>Dismiss</Button>
              </div>
            )}

            {/* Scrollable message history — content scrolls underneath the floating composer */}
            <ScrollArea className="min-h-0 flex-1">
              <div className="mx-auto max-w-3xl space-y-4 px-4 pt-4 pb-40">
                {messages.length === 0 && (
                  <div className="flex h-[40vh] items-center justify-center text-center text-sm text-muted-foreground">
                    {enabledModels.length === 0
                      ? "Add a model with an enabled provider endpoint on the Models page, then start chatting."
                      : "Pick a model in the composer below, then start chatting."}
                  </div>
                )}
                {messages.map((msg, i) => (
                  <div key={i} className={`flex ${msg.role === "user" ? "justify-end" : "justify-start"}`}>
                    <div className={`max-w-[80%] rounded-lg p-3 ${
                      msg.role === "user"
                        ? "bg-primary text-primary-foreground"
                        : "bg-muted"
                    }`}>
                      <div className="mb-1 text-xs font-medium">
                        {msg.role === "user" ? "You" : "Assistant"}
                      </div>
                      <div className="text-sm whitespace-pre-wrap">{msg.content}</div>
                    </div>
                  </div>
                ))}
                {loading && (
                  <div className="flex justify-start">
                    <div className="rounded-lg bg-muted p-3">
                      <div className="mb-1 text-xs font-medium">Assistant</div>
                      <div className="text-sm text-muted-foreground">Thinking…</div>
                    </div>
                  </div>
                )}
                <div ref={messagesEndRef} />
              </div>
            </ScrollArea>

            {/* Floating composer pinned to the bottom of the page */}
            <div className="pointer-events-none absolute inset-x-0 bottom-0 flex justify-center px-4 pb-6">
              <div className="pointer-events-auto w-full max-w-3xl rounded-2xl border bg-background/95 shadow-lg backdrop-blur supports-[backdrop-filter]:bg-background/80">
                {/* Top panel: model selection (left) + context badges (right) */}
                <div className="flex items-center gap-2 border-b px-2 py-1.5">
                  <Select value={selectedModel} onValueChange={setSelectedModel}>
                    <SelectTrigger
                      size="sm"
                      className="h-7 w-auto min-w-[150px] border-0 bg-transparent px-2 shadow-none hover:bg-muted focus-visible:ring-0"
                    >
                      <SelectValue placeholder="Select model" />
                    </SelectTrigger>
                    <SelectContent>
                      {enabledModels.length === 0 && (
                        <SelectItem value="__none__" disabled>No enabled models</SelectItem>
                      )}
                      {enabledModels.map((m) => (
                        <SelectItem key={m.id} value={m.name}>{m.display_name || m.name}</SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                  <div className="ml-auto flex items-center gap-1.5">
                    <Badge variant="secondary" className="font-normal tabular-nums" aria-label="Context usage">
                      <HugeiconsIcon icon={DashboardSpeed01Icon} strokeWidth={2} />
                      ~{usedTokens.toLocaleString()} / {formatTokens(CONTEXT_WINDOW)}
                    </Badge>
                    <Badge variant="secondary" className="font-normal tabular-nums" aria-label="Messages exchanged">
                      <HugeiconsIcon icon={BubbleChatIcon} strokeWidth={2} />
                      {messages.length}
                    </Badge>
                  </div>
                </div>

                {/* Input with a floating send button inside the text area */}
                <div className="relative">
                  <Textarea
                    value={input}
                    onChange={(e) => setInput(e.target.value)}
                    onKeyDown={handleKeyDown}
                    placeholder={enabledModels.length === 0 ? "No enabled models available" : "Message the model…"}
                    disabled={loading || enabledModels.length === 0}
                    rows={1}
                    className="max-h-40 min-h-[56px] resize-none border-0 bg-transparent px-4 py-3 pr-14 shadow-none focus-visible:ring-0"
                  />
                  <Button
                    size="icon"
                    className="absolute bottom-2.5 right-2.5 rounded-full"
                    onClick={sendMessage}
                    disabled={!canSend}
                    aria-label="Send message"
                  >
                    <HugeiconsIcon icon={SentIcon} strokeWidth={2} />
                  </Button>
                </div>
              </div>
            </div>
          </div>
        </SidebarInset>
      </SidebarProvider>
    </AuthGuard>
  )
}
