"use client"

import { useEffect, useState, useRef } from "react"
import { api, type Model, type ApiKey } from "@/lib/api"
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
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Badge } from "@/components/ui/badge"

interface Message {
  role: "user" | "assistant" | "system"
  content: string
}

export default function PlaygroundPage() {
  const [models, setModels] = useState<Model[]>([])
  const [keys, setKeys] = useState<ApiKey[]>([])
  const [selectedModel, setSelectedModel] = useState("")
  const [selectedKey, setSelectedKey] = useState("")
  const [messages, setMessages] = useState<Message[]>([])
  const [input, setInput] = useState("")
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const messagesEndRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    api.listModels().then(setModels).catch((e) => setError(e.message))
    api.listKeys().then(setKeys).catch((e) => setError(e.message))
  }, [])

  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: "smooth" })
  }, [messages])

  const enabledModels = models.filter((m) => m.enabled)

  const sendMessage = async () => {
    if (!input.trim() || !selectedModel || !selectedKey || loading) return

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
            Authorization: `Bearer ${selectedKey}`,
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

  const clearChat = () => {
    setMessages([])
    setError(null)
  }

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
          <div className="flex flex-1 flex-col gap-4 p-4 pt-0">
            {error && (
              <div className="rounded-md bg-destructive/10 p-3 text-sm text-destructive">
                {error}
                <Button variant="ghost" size="sm" className="ml-2" onClick={() => setError(null)}>Dismiss</Button>
              </div>
            )}

            {/* Configuration */}
            <Card>
              <CardHeader>
                <CardTitle className="text-sm">Configuration</CardTitle>
              </CardHeader>
              <CardContent>
                <div className="grid gap-4 md:grid-cols-2">
                  <div>
                    <Label>Model</Label>
                    <select
                      value={selectedModel}
                      onChange={(e) => setSelectedModel(e.target.value)}
                      className="flex h-9 w-full rounded-md border border-input bg-transparent px-3 py-1 text-sm shadow-sm"
                    >
                      <option value="">Select model</option>
                      {enabledModels.map((m) => (
                        <option key={m.id} value={m.name}>{m.display_name || m.name}</option>
                      ))}
                    </select>
                  </div>
                  <div>
                    <Label>API Key</Label>
                    <select
                      value={selectedKey}
                      onChange={(e) => setSelectedKey(e.target.value)}
                      className="flex h-9 w-full rounded-md border border-input bg-transparent px-3 py-1 text-sm shadow-sm"
                    >
                      <option value="">Select key</option>
                      {keys.filter((k) => k.enabled).map((k) => (
                        <option key={k.id} value={k.key}>{k.name}</option>
                      ))}
                    </select>
                  </div>
                </div>
              </CardContent>
            </Card>

            {/* Chat Area */}
            <Card className="flex-1 flex flex-col">
              <CardHeader className="flex flex-row items-center justify-between">
                <CardTitle className="text-sm">Chat</CardTitle>
                <div className="flex items-center gap-2">
                  <Badge variant="outline">{messages.length} messages</Badge>
                  <Button variant="outline" size="sm" onClick={clearChat}>Clear</Button>
                </div>
              </CardHeader>
              <CardContent className="flex-1 flex flex-col gap-4 overflow-hidden">
                {/* Messages */}
                <div className="flex-1 overflow-auto space-y-4 min-h-[300px] max-h-[500px]">
                  {messages.length === 0 && (
                    <div className="flex items-center justify-center h-full text-muted-foreground">
                      Select a model and API key, then start chatting
                    </div>
                  )}
                  {messages.map((msg, i) => (
                    <div key={i} className={`flex ${msg.role === "user" ? "justify-end" : "justify-start"}`}>
                      <div className={`max-w-[80%] rounded-lg p-3 ${
                        msg.role === "user"
                          ? "bg-primary text-primary-foreground"
                          : "bg-muted"
                      }`}>
                        <div className="text-xs font-medium mb-1">
                          {msg.role === "user" ? "You" : "Assistant"}
                        </div>
                        <div className="text-sm whitespace-pre-wrap">{msg.content}</div>
                      </div>
                    </div>
                  ))}
                  {loading && (
                    <div className="flex justify-start">
                      <div className="bg-muted rounded-lg p-3">
                        <div className="text-xs font-medium mb-1">Assistant</div>
                        <div className="text-sm text-muted-foreground">Thinking...</div>
                      </div>
                    </div>
                  )}
                  <div ref={messagesEndRef} />
                </div>

                {/* Input */}
                <div className="flex gap-2">
                  <Input
                    value={input}
                    onChange={(e) => setInput(e.target.value)}
                    onKeyDown={handleKeyDown}
                    placeholder={selectedModel && selectedKey ? "Type a message..." : "Select model and key first"}
                    disabled={!selectedModel || !selectedKey || loading}
                  />
                  <Button
                    onClick={sendMessage}
                    disabled={!input.trim() || !selectedModel || !selectedKey || loading}
                  >
                    Send
                  </Button>
                </div>
              </CardContent>
            </Card>
          </div>
        </SidebarInset>
      </SidebarProvider>
    </AuthGuard>
  )
}
