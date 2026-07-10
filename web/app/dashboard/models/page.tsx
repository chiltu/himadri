"use client"

import { Fragment, useEffect, useState, useCallback } from "react"
import {
  api,
  type Model,
  type ModelEndpoint,
  type CreateModelEndpointRequest,
} from "@/lib/api"
import { AuthGuard } from "@/components/auth-guard"
import { AppSidebar } from "@/components/app-sidebar"
import {
  Breadcrumb,
  BreadcrumbItem,
  BreadcrumbList,
  BreadcrumbPage,
} from "@/components/ui/breadcrumb"
import { Separator } from "@/components/ui/separator"
import {
  SidebarInset,
  SidebarProvider,
  SidebarTrigger,
} from "@/components/ui/sidebar"
import { Button } from "@/components/ui/button"
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
  DialogFooter,
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

// Fallback only — the authoritative list is served by the gateway on
// GET /admin/known-providers (see loadAll), so the picker can't drift from
// what the gateway actually routes.
const FALLBACK_PROVIDER_TYPES = [
  "openai",
  "anthropic",
  "gemini",
  "openrouter",
  "together",
  "groq",
  "fireworks",
  "deepinfra",
  "cerebras",
  "novita",
]

interface ModelFormState {
  name: string
  displayName: string
}

const emptyModelForm: ModelFormState = { name: "", displayName: "" }

interface EndpointFormState {
  providerType: string
  baseUrl: string
  apiKey: string
  weight: string
}

const emptyEndpointForm: EndpointFormState = {
  providerType: "",
  baseUrl: "",
  apiKey: "",
  weight: "",
}

export default function ModelsPage() {
  const [models, setModels] = useState<Model[]>([])
  const [endpointsByModel, setEndpointsByModel] = useState<Record<string, ModelEndpoint[]>>({})
  const [expanded, setExpanded] = useState<Set<string>>(new Set())
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)

  const [addModelOpen, setAddModelOpen] = useState(false)
  const [modelForm, setModelForm] = useState<ModelFormState>(emptyModelForm)

  const [addEndpointFor, setAddEndpointFor] = useState<Model | null>(null)
  const [endpointForm, setEndpointForm] = useState<EndpointFormState>(emptyEndpointForm)
  const [providerTypes, setProviderTypes] = useState<string[]>(FALLBACK_PROVIDER_TYPES)

  const loadAll = useCallback(async () => {
    try {
      const ms = await api.listModels()
      setModels(ms)
      const pairs = await Promise.all(
        ms.map(async (m) => [m.id, await api.listEndpoints(m.id)] as const),
      )
      setEndpointsByModel(Object.fromEntries(pairs))
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : "Failed to load models")
    }
    // Older gateways don't serve this route; keep the fallback list then.
    try {
      const types = await api.knownProviders()
      if (types.length > 0) setProviderTypes(types)
    } catch {
      /* keep FALLBACK_PROVIDER_TYPES */
    }
  }, [])

  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect
    loadAll()
  }, [loadAll])

  const endpointsFor = (modelId: string) => endpointsByModel[modelId] ?? []
  const isActive = (modelId: string) => endpointsFor(modelId).some((e) => e.enabled)

  const toggleExpanded = (id: string) =>
    setExpanded((prev) => {
      const next = new Set(prev)
      if (next.has(id)) next.delete(id)
      else next.add(id)
      return next
    })

  // ---------- Model actions ----------

  const handleCreateModel = async () => {
    setBusy(true)
    try {
      await api.createModel({
        name: modelForm.name,
        display_name: modelForm.displayName || undefined,
      })
      setAddModelOpen(false)
      setModelForm(emptyModelForm)
      loadAll()
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : "Failed to create model")
    } finally {
      setBusy(false)
    }
  }

  const handleToggleModel = async (model: Model) => {
    try {
      await api.toggleModel(model.id, !model.enabled)
      loadAll()
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : "Failed to toggle model")
    }
  }

  const handleDeleteModel = async (model: Model) => {
    if (!confirm(`Delete model "${model.name}"? This fails if the model is enabled.`)) return
    try {
      await api.deleteModel(model.id)
      loadAll()
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : "Failed to delete model")
    }
  }

  // ---------- Endpoint actions ----------

  const openAddEndpoint = (model: Model) => {
    setEndpointForm(emptyEndpointForm)
    setAddEndpointFor(model)
  }

  const handleCreateEndpoint = async () => {
    if (!addEndpointFor) return
    setBusy(true)
    try {
      const req: CreateModelEndpointRequest = {
        provider_type: endpointForm.providerType.trim(),
        base_url: endpointForm.baseUrl.trim() || undefined,
        api_key: endpointForm.apiKey || undefined,
        weight: endpointForm.weight ? Number(endpointForm.weight) : undefined,
      }
      await api.createEndpoint(addEndpointFor.id, req)
      setExpanded((prev) => new Set(prev).add(addEndpointFor.id))
      setAddEndpointFor(null)
      loadAll()
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : "Failed to add provider endpoint")
    } finally {
      setBusy(false)
    }
  }

  const handleToggleEndpoint = async (endpoint: ModelEndpoint) => {
    try {
      await api.toggleEndpoint(endpoint.id, !endpoint.enabled)
      loadAll()
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : "Failed to toggle endpoint")
    }
  }

  const handleDeleteEndpoint = async (endpoint: ModelEndpoint) => {
    if (!confirm(`Remove the ${endpoint.provider_type} endpoint from this model?`)) return
    try {
      await api.deleteEndpoint(endpoint.id)
      loadAll()
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : "Failed to delete endpoint")
    }
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
                  <BreadcrumbItem>
                    <BreadcrumbPage>Models</BreadcrumbPage>
                  </BreadcrumbItem>
                </BreadcrumbList>
              </Breadcrumb>
            </div>
          </header>
          <div className="flex flex-1 flex-col gap-6 p-4 pt-0">
            {error && (
              <div className="rounded-md bg-destructive/10 p-3 text-sm text-destructive">
                {error}
                <Button variant="ghost" size="sm" className="ml-2" onClick={() => setError(null)}>Dismiss</Button>
              </div>
            )}

            <div className="flex items-center justify-between">
              <div>
                <h1 className="text-lg font-semibold">Models</h1>
                <p className="text-sm text-muted-foreground">
                  Onboard a model, then attach one or more provider endpoints. A model is
                  active (routable) only once it has an enabled endpoint.
                </p>
              </div>
              <Dialog open={addModelOpen} onOpenChange={setAddModelOpen}>
                <DialogTrigger asChild>
                  <Button>Add Model</Button>
                </DialogTrigger>
                <DialogContent>
                  <DialogHeader>
                    <DialogTitle>Add Model</DialogTitle>
                  </DialogHeader>
                  <div className="space-y-4">
                    <div className="space-y-2">
                      <Label htmlFor="m-name">Model Name</Label>
                      <Input id="m-name" value={modelForm.name} onChange={(e) => setModelForm((f) => ({ ...f, name: e.target.value }))} placeholder="e.g. gpt-4o" />
                    </div>
                    <div className="space-y-2">
                      <Label htmlFor="m-display">Display Name (optional)</Label>
                      <Input id="m-display" value={modelForm.displayName} onChange={(e) => setModelForm((f) => ({ ...f, displayName: e.target.value }))} placeholder="e.g. GPT-4o" />
                    </div>
                  </div>
                  <DialogFooter>
                    <Button onClick={handleCreateModel} disabled={busy || !modelForm.name.trim()} className="w-full">
                      Create
                    </Button>
                  </DialogFooter>
                </DialogContent>
              </Dialog>
            </div>

            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Model</TableHead>
                  <TableHead>Display Name</TableHead>
                  <TableHead>Status</TableHead>
                  <TableHead>Endpoints</TableHead>
                  <TableHead className="text-right">Actions</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {models.map((m) => {
                  const eps = endpointsFor(m.id)
                  const active = isActive(m.id)
                  return (
                    <Fragment key={m.id}>
                      <TableRow>
                        <TableCell className="font-mono font-medium">
                          <button className="hover:underline" onClick={() => toggleExpanded(m.id)}>
                            {expanded.has(m.id) ? "▾" : "▸"} {m.name}
                          </button>
                        </TableCell>
                        <TableCell>{m.display_name || "-"}</TableCell>
                        <TableCell>
                          <Badge variant={active ? "default" : "secondary"}>
                            {active ? "Active" : "Inactive"}
                          </Badge>
                          {!m.enabled && <Badge variant="outline" className="ml-1">disabled</Badge>}
                        </TableCell>
                        <TableCell className="text-muted-foreground">
                          {eps.filter((e) => e.enabled).length}/{eps.length}
                        </TableCell>
                        <TableCell className="text-right">
                          <Button variant="outline" size="sm" className="mr-2" onClick={() => openAddEndpoint(m)}>
                            Add provider
                          </Button>
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
                      {expanded.has(m.id) && (
                        <TableRow key={`${m.id}-endpoints`}>
                          <TableCell colSpan={5} className="bg-muted/30">
                            {eps.length === 0 ? (
                              <p className="text-sm text-muted-foreground py-2">
                                No provider endpoints yet — this model is inactive. Use “Add provider”.
                              </p>
                            ) : (
                              <Table>
                                <TableHeader>
                                  <TableRow>
                                    <TableHead>Provider</TableHead>
                                    <TableHead>Base URL</TableHead>
                                    <TableHead>Weight</TableHead>
                                    <TableHead>Status</TableHead>
                                    <TableHead className="text-right">Actions</TableHead>
                                  </TableRow>
                                </TableHeader>
                                <TableBody>
                                  {eps.map((ep) => (
                                    <TableRow key={ep.id}>
                                      <TableCell className="font-medium">{ep.provider_type}</TableCell>
                                      <TableCell className="font-mono text-xs text-muted-foreground">{ep.base_url || "(preset)"}</TableCell>
                                      <TableCell className="text-muted-foreground">{ep.weight}</TableCell>
                                      <TableCell>
                                        <Badge variant={ep.enabled ? "default" : "secondary"}>
                                          {ep.enabled ? "Enabled" : "Disabled"}
                                        </Badge>
                                      </TableCell>
                                      <TableCell className="text-right">
                                        <Button variant="ghost" size="sm" onClick={() => handleToggleEndpoint(ep)}>
                                          {ep.enabled ? "Disable" : "Enable"}
                                        </Button>
                                        <Button variant="ghost" size="sm" className="text-destructive" onClick={() => handleDeleteEndpoint(ep)}>
                                          Remove
                                        </Button>
                                      </TableCell>
                                    </TableRow>
                                  ))}
                                </TableBody>
                              </Table>
                            )}
                          </TableCell>
                        </TableRow>
                      )}
                    </Fragment>
                  )
                })}
                {models.length === 0 && (
                  <TableRow>
                    <TableCell colSpan={5} className="text-center text-muted-foreground py-8">
                      No models yet — click “Add Model” to onboard one.
                    </TableCell>
                  </TableRow>
                )}
              </TableBody>
            </Table>
          </div>
        </SidebarInset>
      </SidebarProvider>

      {/* Add provider endpoint */}
      <Dialog open={addEndpointFor !== null} onOpenChange={(open) => !open && setAddEndpointFor(null)}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Add provider endpoint{addEndpointFor ? ` — ${addEndpointFor.name}` : ""}</DialogTitle>
          </DialogHeader>
          <div className="space-y-4">
            <div className="space-y-2">
              <Label htmlFor="e-type">Provider Type</Label>
              <Input
                id="e-type"
                list="provider-types"
                value={endpointForm.providerType}
                onChange={(e) => setEndpointForm((f) => ({ ...f, providerType: e.target.value }))}
                placeholder="e.g. openai, anthropic, openrouter"
              />
              <datalist id="provider-types">
                {providerTypes.map((t) => (
                  <option key={t} value={t} />
                ))}
              </datalist>
              <p className="text-xs text-muted-foreground">
                A known type uses its preset base URL. Any other type requires a Base URL below.
              </p>
            </div>
            <div className="space-y-2">
              <Label htmlFor="e-base">Base URL (optional for known types)</Label>
              <Input id="e-base" value={endpointForm.baseUrl} onChange={(e) => setEndpointForm((f) => ({ ...f, baseUrl: e.target.value }))} placeholder="https://api.openai.com/v1" />
            </div>
            <div className="space-y-2">
              <Label htmlFor="e-key">API Key</Label>
              <Input id="e-key" type="password" value={endpointForm.apiKey} onChange={(e) => setEndpointForm((f) => ({ ...f, apiKey: e.target.value }))} placeholder="sk-..." />
            </div>
            <div className="space-y-2">
              <Label htmlFor="e-weight">Routing Weight (optional)</Label>
              <Input id="e-weight" type="number" value={endpointForm.weight} onChange={(e) => setEndpointForm((f) => ({ ...f, weight: e.target.value }))} placeholder="1" />
              <p className="text-xs text-muted-foreground">Higher wins weighted load-balancing; lower wins cost-optimized routing. Defaults to 1.</p>
            </div>
          </div>
          <DialogFooter>
            <Button
              onClick={handleCreateEndpoint}
              disabled={busy || !endpointForm.providerType.trim()}
              className="w-full"
            >
              Add endpoint
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </AuthGuard>
  )
}
