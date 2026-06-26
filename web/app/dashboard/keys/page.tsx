"use client"

import { AuthGuard } from "@/components/auth-guard"
import { useEffect, useState, useCallback } from "react"
import { api, type ApiKey, type CreateApiKeyRequest } from "@/lib/api"
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

export default function KeysPage() {
  const [keys, setKeys] = useState<ApiKey[]>([])
  const [error, setError] = useState<string | null>(null)
  const [createOpen, setCreateOpen] = useState(false)
  const [newKeyName, setNewKeyName] = useState("")
  const [newKeyScopes, setNewKeyScopes] = useState("api")
  const [createdKey, setCreatedKey] = useState<ApiKey | null>(null)

  const loadKeys = useCallback(() => {
    api.listKeys().then(setKeys).catch((e) => setError(e.message))
  }, [])

  useEffect(() => {
    loadKeys()
  }, [loadKeys])

  const handleCreate = async () => {
    try {
      const req: CreateApiKeyRequest = {
        name: newKeyName,
        scopes: newKeyScopes.split(",").map((s) => s.trim()),
      }
      const key = await api.createKey(req)
      setCreatedKey(key)
      setCreateOpen(false)
      setNewKeyName("")
      setNewKeyScopes("api")
      loadKeys()
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : "Failed to create key")
    }
  }

  const handleRevoke = async (id: string) => {
    if (!confirm("Revoke this key? It will stop working immediately.")) return
    try {
      await api.revokeKey(id)
      loadKeys()
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : "Failed to revoke key")
    }
  }

  const handleRotate = async (id: string) => {
    if (!confirm("Rotate this key? The old key will be invalidated.")) return
    try {
      const newKey = await api.rotateKey(id)
      setCreatedKey(newKey)
      loadKeys()
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : "Failed to rotate key")
    }
  }

  const handleDelete = async (id: string) => {
    if (!confirm("Permanently delete this key?")) return
    try {
      await api.deleteKey(id)
      loadKeys()
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : "Failed to delete key")
    }
  }

  const handleToggle = async (key: ApiKey) => {
    try {
      await api.updateKey(key.id, { enabled: !key.enabled })
      loadKeys()
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : "Failed to update key")
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
                <BreadcrumbItem className="hidden md:block">
                  <BreadcrumbLink href="#">API Gateway</BreadcrumbLink>
                </BreadcrumbItem>
                <BreadcrumbSeparator className="hidden md:block" />
                <BreadcrumbItem>
                  <BreadcrumbPage>All Keys</BreadcrumbPage>
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

          {createdKey && (
            <div className="rounded-md border border-green-500 bg-green-500/10 p-4">
              <p className="text-sm font-medium text-green-600 mb-2">Key Created — Copy it now, it won&apos;t be shown again.</p>
              <code className="block rounded bg-muted p-3 font-mono text-sm break-all">{createdKey.key}</code>
              <Button variant="outline" size="sm" className="mt-2" onClick={() => navigator.clipboard.writeText(createdKey.key)}>
                Copy to Clipboard
              </Button>
            </div>
          )}

          <Card>
            <CardHeader>
              <div className="flex items-center justify-between">
                <CardTitle>All Keys</CardTitle>
                <Dialog open={createOpen} onOpenChange={setCreateOpen}>
                  <DialogTrigger asChild>
                    <Button>Create Key</Button>
                  </DialogTrigger>
                  <DialogContent>
                    <DialogHeader>
                      <DialogTitle>Create API Key</DialogTitle>
                    </DialogHeader>
                    <div className="space-y-4">
                      <div>
                        <Label>Name</Label>
                        <Input value={newKeyName} onChange={(e) => setNewKeyName(e.target.value)} placeholder="e.g. production-key" />
                      </div>
                      <div>
                        <Label>Scopes (comma-separated)</Label>
                        <Input value={newKeyScopes} onChange={(e) => setNewKeyScopes(e.target.value)} placeholder="api, admin" />
                      </div>
                      <Button onClick={handleCreate} className="w-full">Create</Button>
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
                    <TableHead>Key</TableHead>
                    <TableHead>Scopes</TableHead>
                    <TableHead>Status</TableHead>
                    <TableHead>Created</TableHead>
                    <TableHead className="text-right">Actions</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {keys.map((k) => (
                    <TableRow key={k.id}>
                      <TableCell className="font-medium">{k.name}</TableCell>
                      <TableCell className="font-mono text-sm">{k.key.slice(0, 8)}...{k.key.slice(-4)}</TableCell>
                      <TableCell>
                        <div className="flex gap-1">
                          {k.scopes.map((s) => (
                            <Badge key={s} variant="secondary">{s}</Badge>
                          ))}
                        </div>
                      </TableCell>
                      <TableCell>
                        <Badge variant={k.enabled ? "default" : "destructive"}>
                          {k.enabled ? "Active" : "Disabled"}
                        </Badge>
                      </TableCell>
                      <TableCell className="text-sm">{new Date(k.created_at).toLocaleDateString()}</TableCell>
                      <TableCell className="text-right">
                        <DropdownMenu>
                          <DropdownMenuTrigger asChild>
                            <Button variant="ghost" size="sm">...</Button>
                          </DropdownMenuTrigger>
                          <DropdownMenuContent align="end">
                            <DropdownMenuItem onClick={() => handleToggle(k)}>
                              {k.enabled ? "Disable" : "Enable"}
                            </DropdownMenuItem>
                            <DropdownMenuItem onClick={() => handleRotate(k.id)}>Rotate Key</DropdownMenuItem>
                            <DropdownMenuItem onClick={() => handleRevoke(k.id)}>Revoke</DropdownMenuItem>
                            <DropdownMenuItem onClick={() => handleDelete(k.id)} className="text-destructive">Delete</DropdownMenuItem>
                          </DropdownMenuContent>
                        </DropdownMenu>
                      </TableCell>
                    </TableRow>
                  ))}
                  {keys.length === 0 && (
                    <TableRow>
                      <TableCell colSpan={6} className="text-center text-muted-foreground py-8">No API keys yet</TableCell>
                    </TableRow>
                  )}
                </TableBody>
              </Table>
            </CardContent>
          </Card>
        </div>
      </SidebarInset>
    </SidebarProvider>
    </AuthGuard>
  )
}
