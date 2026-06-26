"use client"

import { useEffect, useState, useCallback } from "react"
import { api, type Provider, type Model, type CreateProviderRequest, type CreateModelRequest } from "@/lib/api"
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
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs"
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table"
import { Badge } from "@/components/ui/badge"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"

export default function ModelsPage() {
  const [providers, setProviders] = useState<Provider[]>([])
  const [models, setModels] = useState<Model[]>([])
  const [error, setError] = useState<string | null>(null)
  const [providerDialogOpen, setProviderDialogOpen] = useState(false)
  const [modelDialogOpen, setModelDialogOpen] = useState(false)

  // Provider form
  const [newProviderName, setNewProviderName] = useState("")
  const [newProviderApiKey, setNewProviderApiKey] = useState("")
  const [newProviderBaseUrl, setNewProviderBaseUrl] = useState("")

  // Model form
  const [newModelName, setNewModelName] = useState("")
  const [newModelDisplayName, setNewModelDisplayName] = useState("")
  const [newModelProviderId, setNewModelProviderId] = useState("")

  const loadProviders = useCallback(() => {
    api.listProviders().then(setProviders).catch((e) => setError(e.message))
  }, [])

  const loadModels = useCallback(() => {
    api.listModels().then(setModels).catch((e) => setError(e.message))
  }, [])

  useEffect(() => {
    loadProviders()
    loadModels()
  }, [loadProviders, loadModels])

  const handleCreateProvider = async () => {
    try {
      const req: CreateProviderRequest = {
        name: newProviderName,
        api_key: newProviderApiKey || undefined,
        base_url: newProviderBaseUrl || undefined,
      }
      await api.createProvider(req)
      setProviderDialogOpen(false)
      setNewProviderName("")
      setNewProviderApiKey("")
      setNewProviderBaseUrl("")
      loadProviders()
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : "Failed to create provider")
    }
  }

  const handleToggleProvider = async (provider: Provider) => {
    try {
      await api.toggleProvider(provider.id, !provider.enabled)
      loadProviders()
      loadModels()
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : "Failed to toggle provider")
    }
  }

  const handleDeleteProvider = async (provider: Provider) => {
    if (!confirm(`Delete provider "${provider.name}"? This will fail if it has models.`)) return
    try {
      await api.deleteProvider(provider.id)
      loadProviders()
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : "Failed to delete provider")
    }
  }

  const handleCreateModel = async () => {
    try {
      const req: CreateModelRequest = {
        name: newModelName,
        provider_id: newModelProviderId,
        display_name: newModelDisplayName || undefined,
      }
      await api.createModel(req)
      setModelDialogOpen(false)
      setNewModelName("")
      setNewModelDisplayName("")
      setNewModelProviderId("")
      loadModels()
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : "Failed to create model")
    }
  }

  const handleToggleModel = async (model: Model) => {
    try {
      await api.toggleModel(model.id, !model.enabled)
      loadModels()
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : "Failed to toggle model")
    }
  }

  const handleDeleteModel = async (model: Model) => {
    if (!confirm(`Delete model "${model.name}"? This will fail if the model is enabled.`)) return
    try {
      await api.deleteModel(model.id)
      loadModels()
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : "Failed to delete model")
    }
  }

  const getProviderName = (providerId: string) => {
    return providers.find((p) => p.id === providerId)?.name || "Unknown"
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
                    <BreadcrumbPage>Models</BreadcrumbPage>
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

            <Tabs defaultValue="providers">
              <TabsList>
                <TabsTrigger value="providers">Providers</TabsTrigger>
                <TabsTrigger value="models">Models</TabsTrigger>
              </TabsList>

              <TabsContent value="providers" className="space-y-4">
                <Card>
                  <CardHeader>
                    <div className="flex items-center justify-between">
                      <CardTitle>Providers</CardTitle>
                      <Dialog open={providerDialogOpen} onOpenChange={setProviderDialogOpen}>
                        <DialogTrigger asChild>
                          <Button>Add Provider</Button>
                        </DialogTrigger>
                        <DialogContent>
                          <DialogHeader>
                            <DialogTitle>Add Provider</DialogTitle>
                          </DialogHeader>
                          <div className="space-y-4">
                            <div>
                              <Label>Name</Label>
                              <Input value={newProviderName} onChange={(e) => setNewProviderName(e.target.value)} placeholder="e.g. openai" />
                            </div>
                            <div>
                              <Label>API Key</Label>
                              <Input type="password" value={newProviderApiKey} onChange={(e) => setNewProviderApiKey(e.target.value)} placeholder="sk-..." />
                            </div>
                            <div>
                              <Label>Base URL (optional)</Label>
                              <Input value={newProviderBaseUrl} onChange={(e) => setNewProviderBaseUrl(e.target.value)} placeholder="https://api.openai.com/v1" />
                            </div>
                            <Button onClick={handleCreateProvider} className="w-full">Create</Button>
                          </div>
                        </DialogContent>
                      </Dialog>
                    </div>
                  </CardHeader>
                  <CardContent>
                    <Table>
                      <TableHeader>
                        <TableRow>
                          <TableHead>Name</TableHead>
                          <TableHead>Status</TableHead>
                          <TableHead>Weight</TableHead>
                          <TableHead>Base URL</TableHead>
                          <TableHead className="text-right">Actions</TableHead>
                        </TableRow>
                      </TableHeader>
                      <TableBody>
                        {providers.map((p) => (
                          <TableRow key={p.id}>
                            <TableCell className="font-medium">{p.name}</TableCell>
                            <TableCell>
                              <Badge variant={p.enabled ? "default" : "secondary"}>
                                {p.enabled ? "Enabled" : "Disabled"}
                              </Badge>
                            </TableCell>
                            <TableCell>{p.weight}</TableCell>
                            <TableCell className="text-sm text-muted-foreground">
                              {p.base_url || "Default"}
                            </TableCell>
                            <TableCell className="text-right">
                              <DropdownMenu>
                                <DropdownMenuTrigger asChild>
                                  <Button variant="ghost" size="sm">...</Button>
                                </DropdownMenuTrigger>
                                <DropdownMenuContent align="end">
                                  <DropdownMenuItem onClick={() => handleToggleProvider(p)}>
                                    {p.enabled ? "Disable" : "Enable"}
                                  </DropdownMenuItem>
                                  <DropdownMenuItem onClick={() => handleDeleteProvider(p)} className="text-destructive">
                                    Delete
                                  </DropdownMenuItem>
                                </DropdownMenuContent>
                              </DropdownMenu>
                            </TableCell>
                          </TableRow>
                        ))}
                        {providers.length === 0 && (
                          <TableRow>
                            <TableCell colSpan={5} className="text-center text-muted-foreground py-8">
                              No providers configured
                            </TableCell>
                          </TableRow>
                        )}
                      </TableBody>
                    </Table>
                  </CardContent>
                </Card>
              </TabsContent>

              <TabsContent value="models" className="space-y-4">
                <Card>
                  <CardHeader>
                    <div className="flex items-center justify-between">
                      <CardTitle>Models</CardTitle>
                      <Dialog open={modelDialogOpen} onOpenChange={setModelDialogOpen}>
                        <DialogTrigger asChild>
                          <Button disabled={providers.length === 0}>Add Model</Button>
                        </DialogTrigger>
                        <DialogContent>
                          <DialogHeader>
                            <DialogTitle>Add Model</DialogTitle>
                          </DialogHeader>
                          <div className="space-y-4">
                            <div>
                              <Label>Model Name</Label>
                              <Input value={newModelName} onChange={(e) => setNewModelName(e.target.value)} placeholder="e.g. gpt-4o" />
                            </div>
                            <div>
                              <Label>Display Name (optional)</Label>
                              <Input value={newModelDisplayName} onChange={(e) => setNewModelDisplayName(e.target.value)} placeholder="e.g. GPT-4o" />
                            </div>
                            <div>
                              <Label>Provider</Label>
                              <select
                                value={newModelProviderId}
                                onChange={(e) => setNewModelProviderId(e.target.value)}
                                className="flex h-9 w-full rounded-md border border-input bg-transparent px-3 py-1 text-sm shadow-sm transition-colors"
                              >
                                <option value="">Select provider</option>
                                {providers.filter((p) => p.enabled).map((p) => (
                                  <option key={p.id} value={p.id}>{p.name}</option>
                                ))}
                              </select>
                            </div>
                            <Button onClick={handleCreateModel} className="w-full" disabled={!newModelProviderId}>Create</Button>
                          </div>
                        </DialogContent>
                      </Dialog>
                    </div>
                  </CardHeader>
                  <CardContent>
                    <Table>
                      <TableHeader>
                        <TableRow>
                          <TableHead>Model</TableHead>
                          <TableHead>Display Name</TableHead>
                          <TableHead>Provider</TableHead>
                          <TableHead>Status</TableHead>
                          <TableHead className="text-right">Actions</TableHead>
                        </TableRow>
                      </TableHeader>
                      <TableBody>
                        {models.map((m) => (
                          <TableRow key={m.id}>
                            <TableCell className="font-medium font-mono">{m.name}</TableCell>
                            <TableCell>{m.display_name || "-"}</TableCell>
                            <TableCell>
                              <Badge variant="outline">{getProviderName(m.provider_id)}</Badge>
                            </TableCell>
                            <TableCell>
                              <Badge variant={m.enabled ? "default" : "secondary"}>
                                {m.enabled ? "Enabled" : "Disabled"}
                              </Badge>
                            </TableCell>
                            <TableCell className="text-right">
                              <DropdownMenu>
                                <DropdownMenuTrigger asChild>
                                  <Button variant="ghost" size="sm">...</Button>
                                </DropdownMenuTrigger>
                                <DropdownMenuContent align="end">
                                  <DropdownMenuItem onClick={() => handleToggleModel(m)}>
                                    {m.enabled ? "Disable" : "Enable"}
                                  </DropdownMenuItem>
                                  <DropdownMenuItem onClick={() => handleDeleteModel(m)} className="text-destructive">
                                    Delete
                                  </DropdownMenuItem>
                                </DropdownMenuContent>
                              </DropdownMenu>
                            </TableCell>
                          </TableRow>
                        ))}
                        {models.length === 0 && (
                          <TableRow>
                            <TableCell colSpan={5} className="text-center text-muted-foreground py-8">
                              {providers.length === 0 ? "Add a provider first" : "No models configured"}
                            </TableCell>
                          </TableRow>
                        )}
                      </TableBody>
                    </Table>
                  </CardContent>
                </Card>
              </TabsContent>
            </Tabs>
          </div>
        </SidebarInset>
      </SidebarProvider>
    </AuthGuard>
  )
}
